//! The shared mapping frames actually travel through.
//!
//! A 1080p frame is eight megabytes. At thirty frames a second that is a quarter of a gigabyte
//! crossing the boundary every second, which rules out sending pixels down the control channel: the
//! helper would spend its time copying and the host would spend its time reading. The helper writes
//! pixels straight into a mapping both processes can see, and the control channel carries only a
//! note saying which generation is ready.
//!
//! # Not tearing
//!
//! Several slots, written in turn, with one atomic counter naming the newest complete one. A reader
//! notes the counter, copies that slot, and checks the counter again: if the writer got far enough
//! ahead to have started overwriting what was being copied, the copy is discarded rather than shown.
//! Detecting that is cheap; preventing it would mean the writer waiting for the reader, and a stalled
//! reader must never be able to stall the session.
//!
//! Three slots is the smallest number that leaves the writer somewhere to work that is neither the
//! frame just published nor the one being read, so in practice the check never fires.
//!
//! # What is in the file
//!
//! The mapping is a file: `/dev/shm` where that exists, the temporary directory otherwise. It is
//! created private to the user, because its contents are a picture of somebody's desktop.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

/// Identifies the layout, so a stale file from another build is not read as this one.
const MAGIC: [u8; 8] = *b"BTFRAME\0";

/// Bumped whenever the header or slot layout changes.
const LAYOUT_VERSION: u32 = 1;

/// Where the slots start. A whole page, so every slot begins page-aligned.
const SLOTS_OFFSET: usize = 4096;

// Header field offsets, written out because two processes have to agree on them.
const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 8;
const OFF_SLOT_COUNT: usize = 12;
const OFF_SLOT_BYTES: usize = 16;
const OFF_PUBLISHED: usize = 24;

/// How many slots a mapping is created with.
///
/// See the module documentation: three is the smallest count that leaves the writer somewhere to
/// work that is neither the newest frame nor the one a reader is copying.
pub const SLOT_COUNT: u32 = 3;

/// A framebuffer shared between the host and a helper process.
#[derive(Debug)]
pub struct SharedFrames {
    /// `None` only while dropping, where the mapping has to be released before the file can go.
    map: Option<MmapMut>,
    path: PathBuf,
    slot_count: u32,
    slot_bytes: u64,
    /// Whether dropping this should delete the file.
    ///
    /// Only the process that created it cleans up. A reader deleting the file would leave the writer
    /// publishing into a mapping nothing can be opened from again.
    owner: bool,
}

impl SharedFrames {
    /// Create a mapping big enough for [`SLOT_COUNT`] frames of `slot_bytes` each.
    ///
    /// `label` only has to be readable; the process id and a counter make the name unique.
    #[allow(unsafe_code)]
    pub fn create(label: &str, slot_bytes: u64) -> io::Result<Self> {
        let path = unique_path(label);
        let total = total_bytes(SLOT_COUNT, slot_bytes)?;

        let file = create_private(&path)?;
        file.set_len(total)?;

        // SAFETY: the file was just created at a path containing this process's id and a private
        // counter, so nothing else has it open, and nothing can open it until the path is sent over
        // the control channel. Every access below stays within the length just set.
        let mut map = unsafe { MmapMut::map_mut(&file) }?;

        map[OFF_MAGIC..OFF_MAGIC + 8].copy_from_slice(&MAGIC);
        write_u32(&mut map, OFF_VERSION, LAYOUT_VERSION);
        write_u32(&mut map, OFF_SLOT_COUNT, SLOT_COUNT);
        write_u64(&mut map, OFF_SLOT_BYTES, slot_bytes);
        write_u64(&mut map, OFF_PUBLISHED, 0);

        Ok(Self {
            map: Some(map),
            path,
            slot_count: SLOT_COUNT,
            slot_bytes,
            owner: true,
        })
    }

    /// Open a mapping another process created.
    #[allow(unsafe_code)]
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().read(true).write(true).open(&path)?;

        // SAFETY: as in `create`. The header is validated immediately below, and every access after
        // that is bounds-checked against the slot count and size it reports.
        let map = unsafe { MmapMut::map_mut(&file) }?;

        if map.len() < SLOTS_OFFSET {
            return Err(invalid("the mapping is too small to hold a header"));
        }
        if map[OFF_MAGIC..OFF_MAGIC + 8] != MAGIC {
            return Err(invalid("the mapping is not a BestTerm framebuffer"));
        }
        let version = read_u32(&map, OFF_VERSION);
        if version != LAYOUT_VERSION {
            return Err(invalid(&format!(
                "the mapping has layout version {version}, this build speaks {LAYOUT_VERSION}"
            )));
        }

        let slot_count = read_u32(&map, OFF_SLOT_COUNT);
        let slot_bytes = read_u64(&map, OFF_SLOT_BYTES);
        // The header came from another process. Believing it and then indexing with it is how a
        // corrupt mapping turns into a read past the end.
        let needed = total_bytes(slot_count, slot_bytes)?;
        if map.len() as u64 != needed {
            return Err(invalid("the mapping does not match its own header"));
        }

        Ok(Self {
            map: Some(map),
            path,
            slot_count,
            slot_bytes,
            owner: false,
        })
    }

    /// Where this mapping can be opened, to be sent over the control channel.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes reserved for one frame.
    pub fn slot_bytes(&self) -> u64 {
        self.slot_bytes
    }

    /// How many slots the mapping holds.
    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }

    /// The newest generation that has been completely written, or 0 if there is none yet.
    pub fn published(&self) -> u64 {
        self.published_cell().load(Ordering::Acquire)
    }

    /// Fill the slot for `generation` and then publish it.
    ///
    /// Generations start at 1, because 0 means "nothing yet". `fill` receives exactly
    /// [`SharedFrames::slot_bytes`] bytes and may write as much of them as the frame needs.
    ///
    /// Publishing happens only after `fill` returns, which is what makes a reader that has seen the
    /// generation certain the pixels behind it are finished.
    pub fn write(&mut self, generation: u64, fill: impl FnOnce(&mut [u8])) -> io::Result<()> {
        if generation == 0 {
            return Err(invalid(
                "generation 0 means 'nothing yet' and cannot be written",
            ));
        }
        let range = self.slot_range(generation)?;
        fill(&mut self.bytes_mut()[range]);
        self.published_cell().store(generation, Ordering::Release);
        Ok(())
    }

    /// Copy the newest complete frame into `out`, taking at most `length` bytes.
    ///
    /// Returns the generation copied, or `None` when nothing has been published yet or the writer
    /// lapped the reader mid-copy. A `None` of the second kind is not an error: the next frame is
    /// already on its way, and showing a torn one would be worse than skipping it.
    pub fn read_latest(&self, length: usize, out: &mut Vec<u8>) -> Option<u64> {
        let generation = self.published();
        if generation == 0 {
            return None;
        }

        let range = self.slot_range(generation).ok()?;
        let slot = &self.bytes()[range];
        let length = length.min(slot.len());

        out.clear();
        out.extend_from_slice(&slot[..length]);

        let intact = copy_is_intact(generation, self.published(), self.slot_count);
        intact.then_some(generation)
    }

    /// Byte range of the slot a generation lives in.
    fn slot_range(&self, generation: u64) -> io::Result<std::ops::Range<usize>> {
        let index = generation % u64::from(self.slot_count);
        let start = SLOTS_OFFSET as u64 + index * self.slot_bytes;
        let end = start + self.slot_bytes;
        match (usize::try_from(start), usize::try_from(end)) {
            (Ok(start), Ok(end)) if end <= self.bytes().len() => Ok(start..end),
            _ => Err(invalid("the slot lies outside the mapping")),
        }
    }

    /// The mapped bytes.
    fn bytes(&self) -> &[u8] {
        self.map
            .as_deref()
            .expect("the mapping is taken only while dropping")
    }

    /// The mapped bytes, for writing.
    fn bytes_mut(&mut self) -> &mut [u8] {
        self.map
            .as_deref_mut()
            .expect("the mapping is taken only while dropping")
    }

    /// The published counter, as something both processes can order their accesses against.
    ///
    /// Plain loads and stores would be a data race between processes: the writer's store of the
    /// generation has to be ordered after its writes to the pixels, and the reader's load has to be
    /// ordered before its reads of them. Acquire and release are exactly that, and nothing else in
    /// this layout needs synchronising.
    #[allow(unsafe_code)]
    fn published_cell(&self) -> &AtomicU64 {
        let base = self.bytes().as_ptr();
        // SAFETY: `OFF_PUBLISHED` sits inside the header, whose presence was checked when the
        // mapping was created or opened. A mapping starts page-aligned and the offset is a multiple
        // of eight, so the pointer is aligned for `u64`. The returned reference borrows `self`, so
        // it cannot outlive the mapping, and nothing ever writes those eight bytes non-atomically.
        unsafe {
            let ptr = base.add(OFF_PUBLISHED).cast::<u64>().cast_mut();
            AtomicU64::from_ptr(ptr)
        }
    }
}

impl Drop for SharedFrames {
    fn drop(&mut self) {
        if !self.owner {
            return;
        }
        // Released before the file is removed: Windows refuses to delete a file that is still
        // mapped, and doing it in this order is the difference between cleaning up and not.
        self.map = None;
        // Still best effort — a reader in another process may hold it open. Leaving a file behind
        // wastes space until the next reboot, which is worth less than a panic in a destructor.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether a copy that began at generation `started` was finished before the writer reached it.
///
/// The writer returns to a slot `slot_count` generations later, and only after publishing everything
/// in between. So the copy is intact exactly while the counter has advanced by less than
/// `slot_count - 1`: at that distance the writer is *in* the slot that was being copied.
fn copy_is_intact(started: u64, now: u64, slot_count: u32) -> bool {
    now.saturating_sub(started) < u64::from(slot_count) - 1
}

/// Total bytes a mapping with these dimensions needs.
fn total_bytes(slot_count: u32, slot_bytes: u64) -> io::Result<u64> {
    if slot_count < 2 {
        return Err(invalid(
            "a mapping needs at least two slots to be read while written",
        ));
    }
    u64::from(slot_count)
        .checked_mul(slot_bytes)
        .and_then(|slots| slots.checked_add(SLOTS_OFFSET as u64))
        .ok_or_else(|| invalid("the requested mapping does not fit in an address space"))
}

/// A path nothing else is using.
fn unique_path(label: &str) -> PathBuf {
    use std::sync::atomic::AtomicU32;
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    // `/dev/shm` is memory; the temporary directory may be a disk. Both work, but writing eight
    // megabytes thirty times a second to a disk-backed file would be a strange thing to do by
    // accident.
    let directory = if Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };

    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let sanitised: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let process = std::process::id();
    directory.join(format!("bestterm-{sanitised}-{process}-{serial}.frames"))
}

/// Create a file only this user can read.
///
/// The contents are a picture of somebody's screen. On a shared machine a world-readable file in
/// `/dev/shm` would be exactly the kind of thing that turns up in a write-up later.
fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    options.open(path)
}

fn invalid(detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail.to_string())
}

fn read_u32(map: &[u8], offset: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&map[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(map: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&map[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn write_u32(map: &mut [u8], offset: usize, value: u32) {
    map[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(map: &mut [u8], offset: usize, value: u64) {
    map[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mapping of `slot_bytes` per frame, deleted when the test ends.
    fn mapping(slot_bytes: u64) -> SharedFrames {
        SharedFrames::create("test", slot_bytes).expect("creates a mapping")
    }

    #[test]
    fn a_fresh_mapping_has_published_nothing() {
        let frames = mapping(64);
        assert_eq!(frames.published(), 0);
        let mut out = Vec::new();
        assert_eq!(frames.read_latest(64, &mut out), None);
    }

    #[test]
    fn a_written_frame_is_read_back_through_a_second_handle() {
        // The point of the whole crate: two independent handles on one mapping, which is what the
        // host and the helper each end up holding.
        let mut writer = mapping(64);
        let reader = SharedFrames::open(writer.path()).expect("opens the same mapping");

        writer
            .write(1, |slot| slot[..4].copy_from_slice(&[1, 2, 3, 4]))
            .expect("writes");

        let mut out = Vec::new();
        assert_eq!(reader.read_latest(4, &mut out), Some(1));
        assert_eq!(out, vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_reader_sees_the_newest_generation_not_the_first() {
        let mut writer = mapping(16);
        let reader = SharedFrames::open(writer.path()).expect("opens");

        for generation in 1..=4u64 {
            writer
                .write(generation, |slot| slot[0] = generation as u8)
                .expect("writes");
        }

        let mut out = Vec::new();
        assert_eq!(reader.read_latest(1, &mut out), Some(4));
        assert_eq!(out, vec![4]);
    }

    #[test]
    fn generations_land_in_different_slots_until_they_wrap() {
        // What makes the tearing check work: consecutive frames must not share a slot.
        let frames = mapping(16);
        let range = |generation| frames.slot_range(generation).expect("in range");

        assert_ne!(range(1), range(2));
        assert_ne!(range(2), range(3));
        assert_eq!(range(1), range(1 + u64::from(SLOT_COUNT)));
    }

    #[test]
    fn a_copy_is_intact_until_the_writer_reaches_the_slot_being_copied() {
        // The arithmetic the whole scheme rests on, checked directly rather than inferred from a
        // race that a test cannot schedule. With three slots the writer is back in the slot that
        // generation g occupies once it has published g + 2.
        assert!(copy_is_intact(5, 5, 3), "no new frame at all");
        assert!(
            copy_is_intact(5, 6, 3),
            "one frame ahead is a different slot"
        );
        assert!(!copy_is_intact(5, 7, 3), "two ahead is this very slot");
        assert!(
            !copy_is_intact(5, 500, 3),
            "far ahead is certainly not intact"
        );

        // A counter that appears to go backwards must not read as a huge gap.
        assert!(
            copy_is_intact(9, 1, 3),
            "a backwards counter is not a lapping"
        );
    }

    #[test]
    fn generation_zero_cannot_be_written() {
        // Zero is the "nothing yet" value. A frame published as generation 0 would be invisible.
        let mut frames = mapping(16);
        assert!(frames.write(0, |_| {}).is_err());
    }

    #[test]
    fn a_reader_racing_a_writer_never_sees_two_frames_mixed_together() {
        // Every frame is one repeated byte, so a copy that caught a slot mid-write comes back with
        // two distinct values in it. That is the corruption the generation counter exists to prevent,
        // and this is the only test here that actually runs the two sides at once.
        const SLOT: usize = 256 * 1024;

        let mut writer = mapping(SLOT as u64);
        let reader = SharedFrames::open(writer.path()).expect("opens");

        let writing = std::thread::spawn(move || {
            for generation in 1..=200u64 {
                writer
                    .write(generation, |slot| slot.fill(generation as u8))
                    .expect("writes");
            }
            writer
        });

        let mut out = Vec::new();
        let mut seen = 0usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while seen < 50 && std::time::Instant::now() < deadline {
            if let Some(generation) = reader.read_latest(SLOT, &mut out) {
                let expected = generation as u8;
                assert!(
                    out.iter().all(|byte| *byte == expected),
                    "generation {generation} came back mixed with another frame"
                );
                seen += 1;
            }
        }
        assert!(seen > 0, "the reader never saw a frame at all");

        // Dropped before the writer, so the owner can remove the file on Windows too.
        drop(reader);
        let _writer = writing.join().expect("the writer finished");
    }

    #[test]
    fn opening_something_that_is_not_a_framebuffer_fails_by_name() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), vec![0u8; SLOTS_OFFSET]).expect("writes");

        let error = SharedFrames::open(file.path()).expect_err("not a framebuffer");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("not a BestTerm framebuffer"),
            "unhelpful message: {error}"
        );
    }

    #[test]
    fn a_header_claiming_more_than_the_file_holds_is_refused() {
        // The header comes from another process. Trusting a slot size it did not have room for is
        // how a corrupt mapping becomes a read past the end.
        let writer = mapping(64);
        let mut bytes = std::fs::read(writer.path()).expect("reads");
        write_u64(&mut bytes, OFF_SLOT_BYTES, 1 << 40);

        let tampered = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(tampered.path(), &bytes).expect("writes");

        let error = SharedFrames::open(tampered.path()).expect_err("claims too much");
        assert!(
            error.to_string().contains("does not match its own header"),
            "unhelpful message: {error}"
        );
    }

    #[test]
    fn a_mapping_from_a_different_layout_version_is_refused() {
        let writer = mapping(64);
        let mut bytes = std::fs::read(writer.path()).expect("reads");
        write_u32(&mut bytes, OFF_VERSION, LAYOUT_VERSION + 1);

        let other = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(other.path(), &bytes).expect("writes");

        let error = SharedFrames::open(other.path()).expect_err("wrong version");
        assert!(
            error.to_string().contains("layout version"),
            "unhelpful message: {error}"
        );
    }

    #[test]
    fn the_owner_deletes_the_file_and_a_reader_does_not() {
        let writer = mapping(16);
        let path = writer.path().to_path_buf();
        let reader = SharedFrames::open(&path).expect("opens");

        drop(reader);
        assert!(path.exists(), "a reader must not delete the mapping");

        drop(writer);
        assert!(!path.exists(), "the owner did not clean up");
    }

    #[test]
    fn a_frame_shorter_than_its_slot_reads_back_at_its_own_length() {
        // Slots are sized for the largest frame a session might carry; a smaller one must not come
        // back padded with whatever the previous frame left behind.
        let mut writer = mapping(1024);
        let reader = SharedFrames::open(writer.path()).expect("opens");

        writer.write(1, |slot| slot.fill(0xAA)).expect("writes");
        let mut out = Vec::new();
        reader.read_latest(10, &mut out).expect("reads");
        assert_eq!(out.len(), 10);
        assert!(out.iter().all(|byte| *byte == 0xAA));
    }

    #[test]
    fn an_impossible_mapping_is_refused_before_it_is_attempted() {
        assert!(total_bytes(SLOT_COUNT, u64::MAX).is_err());
        assert!(
            total_bytes(1, 1024).is_err(),
            "one slot cannot be read while written"
        );
        assert!(total_bytes(0, 1024).is_err());
    }
}
