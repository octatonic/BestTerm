//! Proving who we are to the server.
//!
//! Credentials arrive as [`Secret`]s from the vault, so a password or passphrase is never held in a
//! plain `String` that a derived `Debug` could print into a log.
//!
//! # The agent is platform-split
//!
//! Three transports, all behind `cfg`: a Unix socket named by `SSH_AUTH_SOCK`, the named pipe of the
//! OpenSSH agent that ships with Windows, and Pageant's channel. The split is handled here rather
//! than leaked to callers — [`Auth::Agent`] means "whatever agent this machine has" — and on Windows
//! both agents are tried, because they are different products rather than two names for one.
//!
//! None of those concrete types appears in a signature. One of them cannot: `russh` uses Pageant's
//! stream internally without re-exporting it, so it is not nameable from here at all. The shared
//! logic is generic over the transport and the type is inferred at each call site.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bestterm_core_vault::Secret;
use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg, load_secret_key};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::transport::{Handler, SshError};

/// How to authenticate.
#[derive(Clone)]
pub enum Auth {
    /// No credential at all, for servers that permit it.
    None,
    /// A password.
    Password(Secret),
    /// A private key on disk.
    PrivateKeyFile {
        /// Path to the key. `~` is expanded, because that is how `ssh_config` writes it.
        path: PathBuf,
        /// Passphrase, when the key has one.
        passphrase: Option<Secret>,
    },
    /// Whatever SSH agent this machine has.
    ///
    /// Tried key by key, in the order the agent offers them, which is the order the user arranged.
    Agent,
    /// Keyboard-interactive: the server asks questions, something answers them.
    ///
    /// This is how most one-time-password and 2FA setups present themselves, so the answers cannot
    /// come from stored configuration — they come from whoever is sitting there, through a
    /// [`PromptResponder`].
    KeyboardInteractive(Arc<dyn PromptResponder>),
}

/// One question from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractivePrompt {
    /// The text to show.
    pub prompt: String,
    /// Whether what is typed should be visible.
    ///
    /// False for a password or a one-time code. Honouring it is the difference between a token
    /// staying private and appearing on someone's screen during a call.
    pub echo: bool,
}

/// Answers the server's questions.
///
/// Implemented by the UI to raise a dialog. Returning `None` means the user gave up, which is
/// reported as [`SshError::AuthenticationCancelled`] rather than as a wrong answer — the two lead to
/// different next steps.
pub trait PromptResponder: Send + Sync {
    /// Answer one round of prompts.
    ///
    /// `name` and `instructions` come from the server and may be empty. Exactly one answer per
    /// prompt must be returned, in order.
    fn respond(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[InteractivePrompt],
    ) -> Option<Vec<Secret>>;
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Written by hand: a derived Debug would put passwords and passphrases in logs.
        match self {
            Self::None => f.write_str("None"),
            Self::Password(_) => f.write_str("Password(<redacted>)"),
            Self::PrivateKeyFile { path, passphrase } => f
                .debug_struct("PrivateKeyFile")
                .field("path", path)
                .field("passphrase", &passphrase.as_ref().map(|_| "<redacted>"))
                .finish(),
            Self::Agent => f.write_str("Agent"),
            Self::KeyboardInteractive(_) => f.write_str("KeyboardInteractive"),
        }
    }
}

/// Run one authentication method to completion.
pub(crate) async fn authenticate(
    handle: &mut Handle<Handler>,
    user: &str,
    auth: &Auth,
) -> Result<(), SshError> {
    let result = match auth {
        Auth::None => handle.authenticate_none(user.to_string()).await?,
        Auth::Password(secret) => {
            handle
                .authenticate_password(user.to_string(), secret.expose().to_string())
                .await?
        }
        Auth::PrivateKeyFile { path, passphrase } => {
            authenticate_with_key(handle, user, path, passphrase.as_ref()).await?
        }
        Auth::Agent => return authenticate_with_agent(handle, user).await,
        Auth::KeyboardInteractive(responder) => {
            return authenticate_interactively(handle, user, responder.as_ref()).await;
        }
    };

    finish(result)
}

/// Turn a russh result into ours.
///
/// `partial_success` is not a failure — the credential was accepted and the server wants another
/// factor. Reporting it as failure sends the user to check something that was right.
fn finish(result: AuthResult) -> Result<(), SshError> {
    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure {
            remaining_methods,
            partial_success,
        } => {
            let remaining = remaining_methods.iter().map(String::from).collect();
            Err(if partial_success {
                SshError::FurtherAuthenticationRequired { remaining }
            } else {
                SshError::AuthenticationFailed { remaining }
            })
        }
    }
}

async fn authenticate_with_key(
    handle: &mut Handle<Handler>,
    user: &str,
    path: &Path,
    passphrase: Option<&Secret>,
) -> Result<AuthResult, SshError> {
    let expanded = expand_home(path);
    let key = load_secret_key(&expanded, passphrase.map(Secret::expose)).map_err(|error| {
        SshError::PrivateKey {
            path: expanded.clone(),
            detail: error.to_string(),
        }
    })?;

    let hash_alg = rsa_hash_alg(&key.algorithm());
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
    Ok(handle.authenticate_publickey(user.to_string(), key).await?)
}

/// Offer every identity the agent holds, in the order it offers them.
///
/// The order is the user's: it is how they arranged their agent. Trying them in some other order
/// would surprise anyone who has put the key they want first.
#[cfg(unix)]
async fn authenticate_with_agent(handle: &mut Handle<Handler>, user: &str) -> Result<(), SshError> {
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|error| SshError::Agent(error.to_string()))?;
    offer_agent_identities(handle, user, &mut agent).await
}

#[cfg(windows)]
async fn authenticate_with_agent(handle: &mut Handle<Handler>, user: &str) -> Result<(), SshError> {
    /// Where the agent that ships with Windows listens.
    const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

    // Windows has two agents in common use and they are different things, not two names for one.
    // OpenSSH's comes with the operating system, so it is tried first; Pageant is PuTTY's and is
    // what a long-time Windows user is more likely to already have running.
    match AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE).await {
        Ok(mut agent) => return offer_agent_identities(handle, user, &mut agent).await,
        Err(error) => {
            tracing::debug!(%error, "no OpenSSH agent pipe; trying Pageant");
        }
    }

    let mut agent = AgentClient::connect_pageant()
        .await
        .map_err(|error| SshError::Agent(error.to_string()))?;
    offer_agent_identities(handle, user, &mut agent).await
}

/// Offer each identity the agent holds until one is accepted.
///
/// Generic over the agent's transport because the concrete types differ per platform — a Unix
/// socket, a Windows named pipe, Pageant's shared memory — and one of them, Pageant's, is not a
/// type this crate can even name: `russh` uses it internally without re-exporting it. Inferring it
/// at the call site sidesteps that entirely.
async fn offer_agent_identities<S>(
    handle: &mut Handle<Handler>,
    user: &str,
    agent: &mut AgentClient<S>,
) -> Result<(), SshError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| SshError::Agent(error.to_string()))?;

    if identities.is_empty() {
        return Err(SshError::AgentHasNoKeys);
    }

    let mut last: Option<SshError> = None;
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            // Certificates from the agent need the certificate authentication path, which is not
            // wired up yet. Skipped rather than mis-offered as a plain key.
            continue;
        };

        let hash_alg = rsa_hash_alg(&key.algorithm());
        // Reborrowed rather than `&mut agent`: `agent` is already a `&mut`, and taking a reference
        // to it would ask for `Signer` on `&mut AgentClient<_>`, which is not what is implemented.
        let result = handle
            .authenticate_publickey_with(user.to_string(), key, hash_alg, &mut *agent)
            .await
            .map_err(|error| SshError::Agent(error.to_string()))?;

        match finish(result) {
            Ok(()) => return Ok(()),
            // Keep the last answer: it carries the methods the server would still accept, which is
            // what the caller shows the user once every identity has been refused.
            Err(error) => last = Some(error),
        }
    }

    Err(last.unwrap_or(SshError::AgentHasNoKeys))
}

/// Answer the server's questions until it is satisfied or refuses.
///
/// The loop matters: a server may ask several rounds — password, then a one-time code — and each
/// round is a fresh set of prompts rather than a repeat of the last.
async fn authenticate_interactively(
    handle: &mut Handle<Handler>,
    user: &str,
    responder: &dyn PromptResponder,
) -> Result<(), SshError> {
    let mut response = handle
        .authenticate_keyboard_interactive_start(user.to_string(), None)
        .await?;

    // A misbehaving server could keep asking forever; the user cannot be expected to notice that
    // they are in a loop, so it is bounded here.
    for _ in 0..MAX_INTERACTIVE_ROUNDS {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods,
                partial_success,
            } => {
                let remaining = remaining_methods.iter().map(String::from).collect();
                return Err(if partial_success {
                    SshError::FurtherAuthenticationRequired { remaining }
                } else {
                    SshError::AuthenticationFailed { remaining }
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let questions: Vec<InteractivePrompt> = prompts
                    .iter()
                    .map(|prompt| InteractivePrompt {
                        prompt: prompt.prompt.clone(),
                        echo: prompt.echo,
                    })
                    .collect();

                let Some(answers) = responder.respond(&name, &instructions, &questions) else {
                    // Giving up is not a wrong answer, and the two lead somewhere different.
                    return Err(SshError::AuthenticationCancelled);
                };

                if answers.len() != questions.len() {
                    return Err(SshError::InteractiveAnswerCount {
                        asked: questions.len(),
                        answered: answers.len(),
                    });
                }

                let answers = answers
                    .iter()
                    .map(|answer| answer.expose().to_string())
                    .collect();
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }

    Err(SshError::TooManyInteractiveRounds)
}

/// How many rounds of questions a server may ask before we assume it is not going to stop.
const MAX_INTERACTIVE_ROUNDS: usize = 32;

/// RSA signatures must name a hash; everything else ignores it.
///
/// Without this an RSA key is offered as `ssh-rsa` with SHA-1, which modern servers refuse outright —
/// and the refusal looks like a wrong key rather than a wrong signature algorithm.
fn rsa_hash_alg(algorithm: &Algorithm) -> Option<HashAlg> {
    match algorithm {
        Algorithm::Rsa { .. } => Some(HashAlg::Sha256),
        _ => None,
    }
}

/// Expand a leading `~`.
///
/// `ssh_config` writes identity paths that way, and neither `load_secret_key` nor the filesystem
/// knows what it means.
pub fn expand_home(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };

    let rest = match text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        Some(rest) => rest,
        // A bare `~` is the home directory itself; `~user` is someone else's and is left alone,
        // because guessing where another account's home lives is worse than failing to open a file.
        None if text == "~" => "",
        None => return path.to_path_buf(),
    };

    match home_directory() {
        Some(home) if rest.is_empty() => home,
        Some(home) => home.join(rest),
        None => path.to_path_buf(),
    }
}

fn home_directory() -> Option<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_reach_a_log_through_debug() {
        let password = Auth::Password(Secret::new("hunter2"));
        assert_eq!(format!("{password:?}"), "Password(<redacted>)");

        let key = Auth::PrivateKeyFile {
            path: PathBuf::from("/keys/id_ed25519"),
            passphrase: Some(Secret::new("open sesame")),
        };
        let printed = format!("{key:?}");
        assert!(!printed.contains("open sesame"), "got {printed}");
        assert!(printed.contains("redacted"), "got {printed}");
        // The path is useful in a log and is not a secret.
        assert!(printed.contains("id_ed25519"), "got {printed}");
    }

    #[test]
    fn an_interactive_responder_is_not_described_in_a_log() {
        struct Never;
        impl PromptResponder for Never {
            fn respond(&self, _: &str, _: &str, _: &[InteractivePrompt]) -> Option<Vec<Secret>> {
                None
            }
        }
        let auth = Auth::KeyboardInteractive(Arc::new(Never));
        assert_eq!(format!("{auth:?}"), "KeyboardInteractive");
    }

    #[test]
    fn a_prompt_carries_whether_the_answer_should_be_visible() {
        // Ignoring `echo` is how a one-time code ends up on screen during a screen share.
        let hidden = InteractivePrompt {
            prompt: "Verification code: ".to_string(),
            echo: false,
        };
        let visible = InteractivePrompt {
            prompt: "Username: ".to_string(),
            echo: true,
        };
        assert!(!hidden.echo);
        assert!(visible.echo);
        assert_ne!(hidden, visible);
    }

    #[test]
    fn a_key_with_no_passphrase_says_so_without_pretending_to_have_one() {
        let key = Auth::PrivateKeyFile {
            path: PathBuf::from("/keys/id_ed25519"),
            passphrase: None,
        };
        assert!(format!("{key:?}").contains("None"));
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        let Some(home) = home_directory() else {
            eprintln!("no home directory in this environment; skipping");
            return;
        };
        assert_eq!(
            expand_home(Path::new("~/.ssh/id_ed25519")),
            home.join(".ssh/id_ed25519")
        );
        assert_eq!(expand_home(Path::new("~")), home);
    }

    #[test]
    fn a_path_without_a_tilde_is_untouched() {
        let absolute = Path::new("/etc/ssh/key");
        assert_eq!(expand_home(absolute), absolute);
        let relative = Path::new("keys/id_rsa");
        assert_eq!(expand_home(relative), relative);
    }

    #[test]
    fn another_users_home_is_left_alone() {
        // Guessing where ~someone else lives is worse than failing to open the file.
        let other = Path::new("~someone/.ssh/id_rsa");
        assert_eq!(expand_home(other), other);
    }

    #[test]
    fn rsa_keys_are_signed_with_sha256_and_others_with_their_own_scheme() {
        // Offering an RSA key without naming a hash means SHA-1, which modern servers refuse — and
        // the refusal looks like a wrong key rather than a wrong signature algorithm.
        assert_eq!(
            rsa_hash_alg(&Algorithm::Rsa { hash: None }),
            Some(HashAlg::Sha256)
        );
        assert_eq!(rsa_hash_alg(&Algorithm::Ed25519), None);
    }
}
