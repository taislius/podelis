//! What keeps an export from filling the disk: the settings as bytes and
//! the one set of rules every export asks, so whatever else writes rows (a
//! team's gateway, say) can hold to the same ones. Nothing here writes.

use std::path::Path;

/// The settings' gigabyte: 1024³, as sizes are shown.
const GB: u64 = 1 << 30;

/// In bytes; 0 turns each off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportLimits {
    /// Stop rather than leave less than this free on the disk.
    pub min_free: u64,
    /// Ask whether to go on each time a file grows by this much.
    pub ask_every: u64,
    /// Stop a file at this size, without asking.
    pub max: u64,
}

/// What an export should do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Go,
    /// It reached a size to ask about.
    Ask,
    /// Over the largest file allowed.
    TooBig,
    /// The disk is down to what's kept free.
    DiskLow,
}

impl ExportLimits {
    pub fn from_gb(min_free: u64, ask_every: u64, max: u64) -> Self {
        let bytes = |gb: u64| gb.saturating_mul(GB);
        ExportLimits { min_free: bytes(min_free), ask_every: bytes(ask_every), max: bytes(max) }
    }

    /// A file of `written` bytes so far, `free` on its disk (`None`: can't
    /// be told), that asks next at `next_ask`. Stopping comes before asking.
    pub fn check(&self, written: u64, free: Option<u64>, next_ask: u64) -> Check {
        if self.max > 0 && written >= self.max {
            Check::TooBig
        } else if free.is_some_and(|free| free < self.min_free) {
            Check::DiskLow
        } else if self.ask_every > 0 && written >= next_ask {
            Check::Ask
        } else {
            Check::Go
        }
    }

    /// Whether a file of the rows shown, `size` bytes, can be written:
    /// within the largest file, and on the disk at all. (The floor is for
    /// exports of every row: a few kB on a small disk shouldn't be refused.)
    pub fn fits_file(&self, size: u64, free: Option<u64>) -> Check {
        if self.max > 0 && size >= self.max {
            Check::TooBig
        } else if free.is_some_and(|free| free < size) {
            Check::DiskLow
        } else {
            Check::Go
        }
    }

    /// Whether an export of every row, `size` bytes, can be written.
    pub fn fits(&self, size: u64, free: Option<u64>) -> Check {
        if self.max > 0 && size >= self.max {
            Check::TooBig
        } else if self.has_room(size, free) {
            Check::Go
        } else {
            Check::DiskLow
        }
    }

    /// Whether the disk keeps what it should with `size` more on it.
    pub fn has_room(&self, size: u64, free: Option<u64>) -> bool {
        free.is_none_or(|free| free.saturating_sub(size) >= self.min_free)
    }

    /// The size to ask at after `written`: the next multiple of `ask_every`.
    pub fn next_ask(&self, written: u64) -> u64 {
        if self.ask_every == 0 {
            return u64::MAX;
        }
        (written / self.ask_every).saturating_add(1).saturating_mul(self.ask_every)
    }
}

/// The room left on the disk `dir` is on, for an unprivileged writer.
/// `None` if that can't be told, which isn't rare and stops nothing: it may
/// not be allowed (a folder on the way we can't search, a container's or
/// sandbox's rules). A full disk is then the write's own error.
#[cfg(unix)]
pub fn free_space(dir: &Path) -> Option<u64> {
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let stat = rustix::fs::statvfs(dir).ok()?;
    usable_space(stat.f_blocks, stat.f_bavail, stat.f_frsize)
}

/// A disk with no blocks at all is one that doesn't say (FUSE and network
/// mounts answer with zeros), not a full one.
#[cfg(unix)]
fn usable_space(blocks: u64, available: u64, block_size: u64) -> Option<u64> {
    if blocks == 0 || block_size == 0 {
        return None;
    }
    available.checked_mul(block_size)
}

#[cfg(not(unix))]
pub fn free_space(_dir: &Path) -> Option<u64> {
    None
}

/// Free space, or nothing if it doesn't come within `wait`: a hung network
/// mount mustn't hold the screen. (The look goes on by itself, and ends with
/// the mount.)
pub fn free_space_within(dir: &Path, wait: std::time::Duration) -> Option<u64> {
    let (tx, rx) = std::sync::mpsc::channel();
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(free_space(&dir));
    });
    rx.recv_timeout(wait).ok().flatten()
}

/// Rows for a page of an export: about 32 MB of them, so the checks between
/// pages hold to that. `row_bytes` is a guess from the loaded rows, doubled
/// for what a format adds.
pub fn page_rows(row_bytes: f64) -> usize {
    const PAGE_BYTES: f64 = 32.0 * 1024.0 * 1024.0;
    if row_bytes <= 0.0 {
        return 10_000;
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
    let rows = (PAGE_BYTES / (row_bytes * 2.0)) as usize;
    rows.clamp(100, 10_000)
}

/// About how many bytes a row takes in a file, from `rows`: its values and
/// a separator each.
pub fn average_row_bytes(rows: &[Vec<Option<String>>]) -> f64 {
    let sample = &rows[..rows.len().min(1000)];
    if sample.is_empty() {
        return 0.0;
    }
    let bytes: usize = sample.iter().flatten().map(|cell| cell.as_ref().map_or(0, String::len) + 1).sum();
    #[expect(clippy::cast_precision_loss, reason = "an estimate")]
    let average = bytes as f64 / sample.len() as f64;
    average
}

/// The bytes on disk so far: the file, and a scratch file beside it.
pub fn written(file: &Path, scratch: &Path) -> u64 {
    let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    size(file).saturating_add(size(scratch))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ExportLimits = ExportLimits { min_free: 5 * GB, ask_every: GB, max: 10 * GB };

    #[test]
    fn stopping_comes_before_asking() {
        let free = Some(100 * GB);
        assert_eq!(LIMITS.check(0, free, GB), Check::Go);
        assert_eq!(LIMITS.check(GB - 1, free, GB), Check::Go);
        assert_eq!(LIMITS.check(GB, free, GB), Check::Ask);
        assert_eq!(LIMITS.check(GB, Some(5 * GB - 1), GB), Check::DiskLow);
        assert_eq!(LIMITS.check(GB, Some(5 * GB), GB), Check::Ask, "exactly what's kept is still enough");
        assert_eq!(LIMITS.check(10 * GB, free, GB), Check::TooBig);
        assert_eq!(LIMITS.check(10 * GB, Some(0), GB), Check::TooBig);
    }

    #[test]
    fn zero_turns_each_off_and_an_unknown_disk_doesnt_stop_it() {
        let off = ExportLimits::from_gb(0, 0, 0);
        assert_eq!(off.check(u64::MAX, Some(0), off.next_ask(0)), Check::Go);
        assert_eq!(off.fits(u64::MAX, Some(0)), Check::Go);
        assert_eq!(off.next_ask(5 * GB), u64::MAX);
        assert_eq!(LIMITS.check(0, None, GB), Check::Go);
        assert_eq!(LIMITS.fits(GB, None), Check::Go);
    }

    #[test]
    fn what_fits_leaves_the_floor_free() {
        assert_eq!(LIMITS.fits(GB, Some(6 * GB)), Check::Go);
        assert_eq!(LIMITS.fits(GB + 1, Some(6 * GB)), Check::DiskLow);
        assert_eq!(LIMITS.fits(0, Some(GB)), Check::DiskLow);
        assert_eq!(LIMITS.fits(u64::MAX, Some(u64::MAX)), Check::TooBig);
        assert_eq!(ExportLimits::from_gb(5, 0, 0).fits(u64::MAX, Some(6 * GB)), Check::DiskLow, "more than the disk");
    }

    #[test]
    fn room_on_the_disk_doesnt_depend_on_the_largest_file() {
        assert!(!LIMITS.has_room(10 * GB, Some(12 * GB)), "over the largest file, and short of room");
        assert!(LIMITS.has_room(10 * GB, Some(15 * GB)));
        assert!(LIMITS.has_room(u64::MAX, None), "unknown stops nothing");
    }

    #[test]
    fn it_asks_at_each_multiple_after_what_was_agreed() {
        assert_eq!(LIMITS.next_ask(0), GB);
        assert_eq!(LIMITS.next_ask(GB - 1), GB);
        assert_eq!(LIMITS.next_ask(GB), 2 * GB);
        assert_eq!(LIMITS.next_ask(3 * GB + 7), 4 * GB);
    }

    #[test]
    fn huge_settings_dont_overflow() {
        assert_eq!(ExportLimits::from_gb(u64::MAX, 0, 0).min_free, u64::MAX);
        let limits = ExportLimits { ask_every: u64::MAX, ..LIMITS };
        assert_eq!(limits.next_ask(u64::MAX), u64::MAX);
    }

    #[test]
    fn a_small_file_of_the_rows_shown_only_has_to_fit() {
        assert_eq!(LIMITS.fits_file(12_000, Some(GB)), Check::Go, "under the floor, and still fine");
        assert_eq!(LIMITS.fits_file(GB + 1, Some(GB)), Check::DiskLow);
        assert_eq!(LIMITS.fits_file(10 * GB, Some(100 * GB)), Check::TooBig);
        assert_eq!(LIMITS.fits_file(GB, None), Check::Go);
        assert_eq!(LIMITS.fits(12_000, Some(GB)), Check::DiskLow, "every row keeps the floor");
    }

    #[test]
    fn a_setting_of_5_gb_is_5_gb_as_shown() {
        assert_eq!(crate::db::human_bytes(ExportLimits::from_gb(5, 0, 0).min_free), "5.0 GB");
    }

    #[test]
    fn pages_are_about_32_mb_of_rows() {
        assert_eq!(page_rows(0.0), 10_000);
        assert_eq!(page_rows(100.0), 10_000, "narrow rows: the most");
        assert_eq!(page_rows(16.0 * 1024.0), 1024);
        assert_eq!(page_rows(10.0 * 1024.0 * 1024.0), 100, "wide rows: the fewest");
    }

    #[test]
    fn a_disk_that_doesnt_answer_in_time_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space_within(dir.path(), std::time::Duration::from_secs(5)).is_some());
        assert_eq!(free_space_within(dir.path(), std::time::Duration::ZERO), None);
    }

    #[test]
    fn a_rows_size_is_its_values_and_a_separator_each() {
        let row = |a: &str| vec![Some(a.to_string()), None];
        assert_eq!(average_row_bytes(&[]), 0.0);
        assert_eq!(average_row_bytes(&[row("abc"), row("a")]), 4.0);
    }

    #[cfg(unix)]
    #[test]
    fn a_disk_that_answers_with_zeros_isnt_full() {
        assert_eq!(usable_space(0, 0, 0), None);
        assert_eq!(usable_space(0, 0, 4096), None);
        assert_eq!(usable_space(100, 0, 0), None);
        assert_eq!(usable_space(100, 0, 4096), Some(0), "one that has blocks, and none free, is");
        assert_eq!(usable_space(100, 10, 4096), Some(40960));
        assert_eq!(usable_space(u64::MAX, u64::MAX, 4096), None, "too big to say");
    }

    #[cfg(unix)]
    #[test]
    fn where_free_space_isnt_allowed_it_says_nothing_and_stops_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::create_dir(locked.join("inner")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // (root isn't kept out by the mode.)
        let kept_out = std::fs::read_dir(&locked).is_err();
        let free = free_space(&locked.join("inner"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if kept_out {
            assert_eq!(free, None);
            assert_eq!(LIMITS.check(GB - 1, free, GB), Check::Go);
            assert_eq!(LIMITS.fits(GB, free), Check::Go);
        }
    }

    #[test]
    fn the_disk_and_a_files_size_can_be_read() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space(dir.path()).is_some_and(|free| free > 0));
        assert_eq!(free_space(Path::new("")).is_some(), free_space(Path::new(".")).is_some());
        assert!(free_space(&dir.path().join("nope")).is_none());
        let (file, scratch) = (dir.path().join("a"), dir.path().join("a.rows"));
        assert_eq!(written(&file, &scratch), 0);
        std::fs::write(&file, "123").unwrap();
        assert_eq!(written(&file, &scratch), 3);
        std::fs::write(&scratch, "45").unwrap();
        assert_eq!(written(&file, &scratch), 5);
    }
}
