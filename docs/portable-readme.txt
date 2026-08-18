BestTerm — portable build
=========================

Run bestterm.exe. Nothing needs installing.

The three binaries have to stay in the same directory. bestterm.exe looks for the
protocol helpers beside itself and deliberately never on PATH: a helper is handed a
password, and something found on PATH is something another program can arrange to
be found. Moving bestterm.exe on its own leaves RDP and VNC reporting that their
helper is missing, with the path it looked in.

  bestterm.exe       the application
  bestterm-rdp.exe   the RDP helper, one process per session
  bestterm-vnc.exe   the VNC helper, likewise

Where it keeps things
---------------------

Configuration and the session tree live under %APPDATA%\BestTerm. Set
BESTTERM_CONFIG_DIR to point that somewhere else — a directory beside this one makes
the whole thing portable, and it is also how to try it without touching an existing
configuration:

  set BESTTERM_CONFIG_DIR=%~dp0config
  bestterm.exe

Command line
------------

  bestterm.exe user@host:port     open an SSH session at startup
  bestterm.exe --import FILE      import a MobaXterm .mxtsessions file

Logging goes to stderr and is off by default beyond warnings. BESTTERM_LOG takes an
env-filter directive:

  set BESTTERM_LOG=debug
  bestterm.exe 2> log.txt

What works
----------

SSH, local shells including WSL, Telnet and Serial are complete. RDP and VNC are
built end to end but have not been run against a real server; RDP has been verified
as far as authentication. Telnet and VNC are not encrypted, and the application says
so when a session opens.

The Split and MultiExec buttons, the Tools and Macros panels, and most menu items
have no behaviour yet. docs/ROADMAP.md in the repository says what is missing and in
what order it is planned.

Licence
-------

GPL-3.0-or-later. See LICENSE.
