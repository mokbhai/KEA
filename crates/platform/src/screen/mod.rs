//! Region screen capture and on-device OCR.
//!
//! Sibling to `permissions/`, `textio/`, `audio/` and `hotkeys/`: a trait, a
//! cfg-selected constructor, a macOS implementation and a non-macOS stub.
//!
//! # Why `/usr/sbin/screencapture -i` and not ScreenCaptureKit
//!
//! The plan asked for this comparison; the answer is the shell-out, and the
//! margin is not close.
//!
//! * **The selector.** `-i` *is* the region selector every Mac user already
//!   knows — crosshair, space to switch to window mode, hover-to-highlight,
//!   Esc to cancel, multi-monitor and Retina handled. SCK gives us none of
//!   that: it captures, it does not select. Building the selector means a
//!   borderless transparent window per display that must not steal focus,
//!   which `src-tauri/src/overlay.rs` shows is days of care, not hours.
//! * **The dependency.** `system-audio-sck` is optional and *nothing enables
//!   it* — `sck_feature_enabled()` returns false in every shipped and dev
//!   build. Choosing SCK here means turning a large native dependency on
//!   across three manifests for the first time, for a feature that is one
//!   `Command::output()` today.
//! * **Permission.** Interactive `screencapture` is user-initiated selection;
//!   SCK is programmatic screen reading and prompts accordingly. See the
//!   verification block in `macos.rs` — this is the one point still to be
//!   confirmed on a machine with a screen.
//!
//! The door stays open: [`ScreenCapture`] is the seam, so an SCK
//! implementation can replace [`macos::MacScreenCapture`] without a caller
//! changing.
//!
//! # Privacy is the dominant risk
//!
//! This module writes a picture of someone's screen to disk. A leaked
//! screenshot sitting in `/tmp` is the worst bug the feature can produce, so
//! the file is owned rather than cleaned up: [`CaptureSlot`] holds a private
//! temp *directory* and removes it (and everything in it) on drop, including
//! on cancel, on error and on panic. No early return cleans up by hand, and
//! nothing outside this module ever learns a path that outlives the
//! [`CapturedImage`] it came from.
//!
//! # Testability
//!
//! Everything that can be decided without a screen is a plain function with
//! tests: [`screencapture_args`], [`classify_capture`], [`png_dimensions`],
//! [`NormalizedRect::to_text_box`], [`reading_order`] and
//! [`observations_to_text`]. The FFI sits at the edges in `macos.rs`.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
pub mod stub;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScreenError {
    /// The platform cannot do this at all, or the tool it needs is missing.
    /// Reported by [`ScreenCapture::availability`] so a missing
    /// `/usr/sbin/screencapture` is a settings-page message rather than a
    /// mid-capture surprise.
    #[error("screen capture unavailable: {0}")]
    Unavailable(String),
    #[error("screen capture failed: {0}")]
    Capture(String),
    #[error("text recognition failed: {0}")]
    Recognize(String),
}

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// Where captures are staged: one private subdirectory of the system temp
/// directory, so a stray file is at least confined to a directory that is ours.
pub(crate) fn capture_parent_dir() -> PathBuf {
    std::env::temp_dir().join("kea-screen")
}

/// A reserved, self-deleting place for a capture that has not happened yet.
///
/// The owner of the cleanup (the house rule: finalization lives in a type, not
/// copy-pasted at each early return). Every exit path out of a capture — the
/// user pressing Esc, the tool failing, a panic — drops the slot, and dropping
/// it removes the whole directory.
#[derive(Debug)]
pub struct CaptureSlot {
    dir: TempDir,
    path: PathBuf,
}

impl CaptureSlot {
    /// Reserve `<parent>/<random>/<file_name>` for a capture.
    ///
    /// The intermediate directory is `mkdtemp`-created, so it is unique and
    /// mode 0700 without us setting permissions by hand. Handing
    /// `screencapture` a path inside a directory we own (rather than a
    /// `NamedTempFile` it would have to overwrite) also sidesteps the question
    /// of whether it truncates an existing file, and guarantees cleanup even
    /// if it ever decided to write a differently-named sibling.
    pub fn claim(parent: &Path, file_name: &str) -> Result<Self, ScreenError> {
        std::fs::create_dir_all(parent).map_err(|e| {
            ScreenError::Capture(format!("cannot create {}: {e}", parent.display()))
        })?;
        let dir = tempfile::Builder::new()
            .prefix("capture-")
            .tempdir_in(parent)
            .map_err(|e| ScreenError::Capture(format!("cannot stage capture: {e}")))?;
        let path = dir.path().join(file_name);
        Ok(Self { dir, path })
    }

    /// The path to hand the capture tool. Nothing exists here yet.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes at [`Self::path`], or `None` when nothing was written.
    ///
    /// The whole of cancel detection reduces to this plus stderr; see
    /// [`classify_capture`].
    pub fn written_bytes(&self) -> Option<u64> {
        std::fs::metadata(&self.path).ok().map(|m| m.len())
    }

    /// Promote a slot that now holds an image, keeping the directory's
    /// ownership of it.
    pub fn finish(self) -> CapturedImage {
        CapturedImage {
            _dir: self.dir,
            path: self.path,
        }
    }
}

/// A screenshot on disk that deletes itself, and the directory holding it,
/// when dropped.
#[derive(Debug)]
pub struct CapturedImage {
    _dir: TempDir,
    path: PathBuf,
}

impl CapturedImage {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The result of asking for a region.
///
/// Cancel is deliberately not a [`ScreenError`]: pressing Esc is the user
/// succeeding at changing their mind, and a caller that treats it as a failure
/// shows an error toast for a non-event.
#[derive(Debug)]
pub enum CaptureOutcome {
    Captured(CapturedImage),
    Cancelled,
}

/// What a finished `screencapture` run means, decided from evidence rather
/// than from its exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureVerdict {
    Captured,
    Cancelled,
    Failed(String),
}

/// Flags for an interactive region capture into `dest`.
///
/// * `-i` interactive selection (the reason we shell out at all).
/// * `-x` no shutter sound — KEA plays its own cues (`audio/cues.rs`), and two
///   sounds for one action is worse than none.
/// * `-o` omit the window's drop shadow in window-selection mode; the shadow
///   is transparent margin that only makes the image bigger.
/// * `-r` omit DPI metadata. Vision reads pixels, so the metadata buys nothing
///   and is one less thing travelling with an image of someone's screen.
/// * `-tpng` force PNG. The default is PNG, but it is a user default
///   (`com.apple.screencapture type`) and we read the PNG header for the image
///   size, so leaving the format to chance would break OCR geometry on a
///   machine configured for JPEG.
///
/// Note: `-o` is the shadow flag and `-r` the metadata flag — the plan
/// attributed both behaviours to `-r`, which the tool's own usage text at HEAD
/// contradicts.
pub fn screencapture_args(dest: &Path) -> Vec<String> {
    vec![
        "-i".into(),
        "-x".into(),
        "-o".into(),
        "-r".into(),
        "-tpng".into(),
        dest.to_string_lossy().into_owned(),
    ]
}

/// Decide what a capture run produced, **without looking at the exit code**.
///
/// The trap the plan flags: `screencapture`'s exit status does not separate
/// "the user pressed Esc" from "something went wrong". Measured at HEAD, a
/// genuine failure (`-R` without Screen Recording) exits 1, prints
/// `could not create image from rect` on stderr *and leaves a zero-byte file
/// behind* — so file size alone cannot tell failure from cancel either, and
/// exit code alone cannot tell cancel from failure. Two signals do:
///
/// * bytes were written → a capture happened, whatever else was said;
/// * nothing written and the tool complained → a real failure, with its own
///   words;
/// * nothing written and nothing said → the user cancelled.
///
/// This holds whichever exit code Esc actually produces, which is the point:
/// it is a gate we cannot close from a headless run, so the logic does not
/// depend on it.
pub fn classify_capture(written_bytes: Option<u64>, stderr: &str) -> CaptureVerdict {
    if written_bytes.unwrap_or(0) > 0 {
        return CaptureVerdict::Captured;
    }
    let complaint = stderr.trim();
    if complaint.is_empty() {
        CaptureVerdict::Cancelled
    } else {
        CaptureVerdict::Failed(complaint.to_string())
    }
}

/// Interactive region capture.
#[async_trait]
pub trait ScreenCapture: Send + Sync {
    /// Cheap readiness probe: `Ok(())` when a capture could be attempted now.
    ///
    /// Separate from [`Self::capture_region`] so the settings page can grey a
    /// row out instead of the user discovering the problem mid-selection.
    fn availability(&self) -> Result<(), ScreenError>;

    /// Let the user select a region. Blocks (asynchronously) for as long as
    /// they take.
    async fn capture_region(&self) -> Result<CaptureOutcome, ScreenError>;
}

/// Construct the active platform [`ScreenCapture`] implementation for this OS.
pub fn new_screen_capture() -> Box<dyn ScreenCapture> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacScreenCapture::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubScreenCapture::new())
    }
}

// ---------------------------------------------------------------------------
// OCR
// ---------------------------------------------------------------------------

/// A box as Vision reports it: normalized to 0..1 of the image, **origin
/// bottom-left**.
///
/// Confirmed on macOS 26 / `VNRecognizeTextRequest` revision 3 against a
/// rendered two-column image: the headline drawn at the top of the image came
/// back with the *largest* `y`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl NormalizedRect {
    /// Convert to pixel space with a top-left origin — the space a person
    /// reads in, and the only space in which "is this gutter wider than that
    /// line gap" is a meaningful question.
    ///
    /// Comparing a normalized x-gap to a normalized y-gap silently compares
    /// different units whenever the image is not square, which is always.
    pub fn to_text_box(&self, image_width: f64, image_height: f64) -> TextBox {
        let left = self.x * image_width;
        let right = (self.x + self.width) * image_width;
        // The Y flip: Vision's y is the box's *bottom* measured up from the
        // image's bottom edge.
        let top = (1.0 - self.y - self.height) * image_height;
        let bottom = (1.0 - self.y) * image_height;
        TextBox {
            left,
            top,
            right,
            bottom,
        }
    }
}

/// A box in image pixels with a top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl TextBox {
    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// One run of recognised text and where it sat on screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub text: String,
    pub bbox: TextBox,
    /// Vision's own 0..1 confidence in the top candidate. Carried, not acted
    /// on: filtering on it drops correct rare words more often than it drops
    /// wrong ones.
    pub confidence: f32,
}

/// Recognition knobs the user can actually see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrOptions {
    /// BCP-47 tags, most preferred first. Empty leaves the choice to Vision.
    pub languages: Vec<String>,
    /// Vision's spell-correction pass. Right for prose, ruinous for code,
    /// identifiers and serial numbers.
    pub language_correction: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            languages: Vec::new(),
            language_correction: true,
        }
    }
}

/// On-device text recognition over a captured image.
#[async_trait]
pub trait TextRecognizer: Send + Sync {
    /// The languages this OS build can actually recognise, asked of the
    /// framework rather than guessed, so the settings page shows a real list.
    fn supported_languages(&self) -> Result<Vec<String>, ScreenError>;

    /// Recognise text in `image`. Observations come back in Vision's order;
    /// callers wanting prose use [`observations_to_text`].
    async fn recognize(
        &self,
        image: &Path,
        opts: &OcrOptions,
    ) -> Result<Vec<Observation>, ScreenError>;
}

/// Construct the active platform [`TextRecognizer`] implementation for this OS.
pub fn new_text_recognizer() -> Box<dyn TextRecognizer> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacTextRecognizer::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubTextRecognizer::new())
    }
}

// ---------------------------------------------------------------------------
// Reading order — the part that is a real bug, and the part that is testable
// ---------------------------------------------------------------------------

/// A column gutter must be at least this many median text heights wide before
/// it is believed. Below that it is word spacing, or the gap in
/// `Label:    value`, and cutting there splits a line in half.
const MIN_GUTTER_TEXT_HEIGHTS: f64 = 1.5;

/// Both sides of a column cut must hold at least this many observations. One
/// observation alone on the far side of a wide gap is a page number or a
/// value, not a column.
const MIN_COLUMN_MEMBERS: usize = 2;

/// Recursion bound for [`reading_order`]. A degenerate layout can peel one
/// observation per cut, so depth is bounded by the observation count; beyond
/// this the fallback (top-to-bottom, then left-to-right) is what a deep
/// single-column recursion would have produced anyway.
const MAX_CUT_DEPTH: usize = 128;

/// Vertical overlap, as a fraction of the shorter box, above which two
/// observations are on the same visual line.
const SAME_LINE_OVERLAP: f64 = 0.5;

/// Put observations into the order a person reads them.
///
/// Vision returns observations with bounding boxes, not document lines, and in
/// no documented order. Joining them as they arrive scrambles any two-column
/// layout — and so, quietly, does the obvious fix of sorting by descending Y
/// then ascending X, which reads *across* columns one row at a time.
///
/// This is a recursive XY-cut: at each step find the widest clean horizontal
/// gap (no box straddles it) and the widest clean vertical gutter, cut on
/// whichever is wider, and recurse. A column gutter is far wider than line
/// leading, so columns are separated before rows are; a headline spanning the
/// full width crosses every gutter, so no column cut is available for the page
/// as a whole and the headline is peeled off first, leaving the body to split
/// into columns. Both directions are measured in pixels, which is why
/// [`NormalizedRect::to_text_box`] happens first.
///
/// Known limitation, pinned by a test: a heading that does *not* reach across
/// the gutter is absorbed into whichever column it overhangs. Its text stays
/// intact and the columns stay unscrambled; only the heading's position moves.
pub fn reading_order(observations: Vec<Observation>) -> Vec<Observation> {
    let order = order_indices(&observations);
    let mut slots: Vec<Option<Observation>> = observations.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

/// Reading-order text: observations sorted, runs sharing a visual line joined
/// with a space, lines joined with newlines.
pub fn observations_to_text(observations: &[Observation]) -> String {
    let order = order_indices(observations);
    assemble_lines(observations, &order).join("\n")
}

fn order_indices(obs: &[Observation]) -> Vec<usize> {
    if obs.len() <= 1 {
        return (0..obs.len()).collect();
    }
    let gutter_floor = median_height(obs) * MIN_GUTTER_TEXT_HEIGHTS;
    let mut out = Vec::with_capacity(obs.len());
    xy_cut(obs, (0..obs.len()).collect(), gutter_floor, 0, &mut out);
    out
}

fn median_height(obs: &[Observation]) -> f64 {
    let mut heights: Vec<f64> = obs.iter().map(|o| o.bbox.height()).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    heights[heights.len() / 2]
}

fn xy_cut(
    obs: &[Observation],
    group: Vec<usize>,
    gutter_floor: f64,
    depth: usize,
    out: &mut Vec<usize>,
) {
    if group.len() <= 1 || depth >= MAX_CUT_DEPTH {
        out.extend(sorted_by_position(obs, group));
        return;
    }

    let rows = split_rows(obs, &group);
    let columns = split_columns(obs, &group, gutter_floor);
    // Wider gap wins: a column gutter dwarfs line leading, so columns separate
    // before rows do. A tie goes to the row cut, because top-to-bottom is the
    // safer guess when the layout gives no reason to prefer columns.
    let prefer_columns = match (
        rows.as_ref().map(|(gap, ..)| *gap),
        columns.as_ref().map(|(gap, ..)| *gap),
    ) {
        (Some(row_gap), Some(col_gap)) => col_gap > row_gap,
        (None, Some(_)) => true,
        _ => false,
    };
    let cut = if prefer_columns { columns } else { rows };

    match cut {
        Some((_, first, second)) => {
            xy_cut(obs, first, gutter_floor, depth + 1, out);
            xy_cut(obs, second, gutter_floor, depth + 1, out);
        }
        None => out.extend(sorted_by_position(obs, group)),
    }
}

/// Top-to-bottom, then left-to-right: the leaf ordering, correct for anything
/// that survived to a leaf without a cut.
fn sorted_by_position(obs: &[Observation], mut group: Vec<usize>) -> Vec<usize> {
    group.sort_by(|&a, &b| {
        let (x, y) = (&obs[a].bbox, &obs[b].bbox);
        match x.top.total_cmp(&y.top) {
            Ordering::Equal => x.left.total_cmp(&y.left),
            other => other,
        }
    });
    group
}

/// The widest horizontal band of empty space that no box straddles.
fn split_rows(obs: &[Observation], group: &[usize]) -> Option<(f64, Vec<usize>, Vec<usize>)> {
    let mut sorted = group.to_vec();
    sorted.sort_by(|&a, &b| obs[a].bbox.top.total_cmp(&obs[b].bbox.top));

    let mut best: Option<(f64, usize)> = None;
    // Running max: everything before `i` ends above `reach`, so a box starting
    // below `reach` is separated from all of them at once.
    let mut reach = obs[sorted[0]].bbox.bottom;
    for i in 1..sorted.len() {
        let bbox = &obs[sorted[i]].bbox;
        let gap = bbox.top - reach;
        if gap > 0.0 && best.is_none_or(|(best_gap, _)| gap > best_gap) {
            best = Some((gap, i));
        }
        reach = reach.max(bbox.bottom);
    }
    best.map(|(gap, i)| (gap, sorted[..i].to_vec(), sorted[i..].to_vec()))
}

/// The widest vertical gutter that no box straddles, if it is wide enough and
/// populated enough to be a column boundary.
fn split_columns(
    obs: &[Observation],
    group: &[usize],
    gutter_floor: f64,
) -> Option<(f64, Vec<usize>, Vec<usize>)> {
    if group.len() < MIN_COLUMN_MEMBERS * 2 {
        return None;
    }
    let mut sorted = group.to_vec();
    sorted.sort_by(|&a, &b| obs[a].bbox.left.total_cmp(&obs[b].bbox.left));

    let mut best: Option<(f64, usize)> = None;
    let mut reach = obs[sorted[0]].bbox.right;
    for i in MIN_COLUMN_MEMBERS..sorted.len().saturating_sub(MIN_COLUMN_MEMBERS - 1) {
        // `reach` must cover everything strictly left of `i`.
        for &j in &sorted[..i] {
            reach = reach.max(obs[j].bbox.right);
        }
        let gap = obs[sorted[i]].bbox.left - reach;
        if gap >= gutter_floor && best.is_none_or(|(best_gap, _)| gap > best_gap) {
            best = Some((gap, i));
        }
    }
    best.map(|(gap, i)| (gap, sorted[..i].to_vec(), sorted[i..].to_vec()))
}

/// Join ordered observations into lines, merging neighbours that share a
/// visual line.
///
/// Only *adjacent* observations are considered, which is what makes this safe
/// in a multi-column layout: the column cut has already put the left column's
/// first line nowhere near the right column's, so they cannot be welded into
/// one line by accident.
fn assemble_lines(obs: &[Observation], order: &[usize]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut previous: Option<&TextBox> = None;

    for &i in order {
        let observation = &obs[i];
        let text = observation.text.trim();
        if text.is_empty() {
            continue;
        }
        match previous {
            Some(prev) if same_line(prev, &observation.bbox) => {
                if let Some(last) = lines.last_mut() {
                    last.push(' ');
                    last.push_str(text);
                }
            }
            _ => lines.push(text.to_string()),
        }
        previous = Some(&observation.bbox);
    }
    lines
}

fn same_line(a: &TextBox, b: &TextBox) -> bool {
    let overlap = a.bottom.min(b.bottom) - a.top.max(b.top);
    let shorter = a.height().min(b.height());
    shorter > 0.0 && overlap > shorter * SAME_LINE_OVERLAP
}

// ---------------------------------------------------------------------------
// PNG header
// ---------------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Pixel dimensions from a PNG's IHDR chunk.
///
/// Vision reports normalized boxes, so the image size is needed to reason
/// about gaps at all. Reading the 24-byte header ourselves keeps that off the
/// FFI path — no ImageIO, no CGImageSource, and it is testable from a byte
/// array. `None` for anything that is not a PNG we wrote, which the caller
/// treats as "assume square" rather than as a failure: bad geometry degrades
/// reading order, it does not lose text.
pub fn png_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    if header.len() < 24 || header[..8] != PNG_SIGNATURE || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(text: &str, left: f64, top: f64, right: f64, bottom: f64) -> Observation {
        Observation {
            text: text.into(),
            bbox: TextBox {
                left,
                top,
                right,
                bottom,
            },
            confidence: 1.0,
        }
    }

    // --- arguments ---------------------------------------------------------

    #[test]
    fn screencapture_args_are_interactive_silent_and_png() {
        let args = screencapture_args(Path::new("/tmp/kea/region.png"));
        assert_eq!(
            args,
            vec!["-i", "-x", "-o", "-r", "-tpng", "/tmp/kea/region.png"]
        );
    }

    #[test]
    fn screencapture_destination_is_the_last_argument() {
        // screencapture takes files positionally after the flags; a path that
        // drifted into the middle would be read as a flag argument.
        let args = screencapture_args(Path::new("/tmp/x.png"));
        assert_eq!(args.last().unwrap(), "/tmp/x.png");
        assert!(args[..args.len() - 1].iter().all(|a| a.starts_with('-')));
    }

    // --- cancel detection --------------------------------------------------

    #[test]
    fn bytes_on_disk_mean_a_capture_happened() {
        assert_eq!(classify_capture(Some(4096), ""), CaptureVerdict::Captured);
    }

    #[test]
    fn a_zero_byte_file_and_a_silent_tool_is_a_cancel() {
        // Esc: screencapture leaves the reserved name untouched and says
        // nothing. This must not surface as an error toast.
        assert_eq!(classify_capture(Some(0), ""), CaptureVerdict::Cancelled);
        assert_eq!(classify_capture(None, "   \n"), CaptureVerdict::Cancelled);
    }

    #[test]
    fn a_zero_byte_file_with_a_complaint_is_a_failure() {
        // Measured at HEAD: a denied capture exits 1, prints this, and leaves a
        // zero-byte file — indistinguishable from cancel by size alone.
        assert_eq!(
            classify_capture(Some(0), "could not create image from rect\n"),
            CaptureVerdict::Failed("could not create image from rect".into())
        );
    }

    #[test]
    fn a_written_image_outranks_a_noisy_tool() {
        // Warnings on stderr must not throw away a capture the user made.
        assert_eq!(
            classify_capture(Some(2048), "some warning"),
            CaptureVerdict::Captured
        );
    }

    // --- the temp file is owned -------------------------------------------

    #[test]
    fn a_slot_reserves_a_private_directory_and_removes_it_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let (dir, path) = {
            let slot = CaptureSlot::claim(parent.path(), "region.png").unwrap();
            assert_eq!(slot.written_bytes(), None, "nothing written yet");
            let dir = slot.path().parent().unwrap().to_path_buf();
            assert!(dir.starts_with(parent.path()));
            (dir, slot.path().to_path_buf())
        };
        assert!(!dir.exists(), "cancel must not leave the directory behind");
        assert!(!path.exists());
    }

    #[test]
    fn a_slot_reports_what_the_tool_wrote() {
        let parent = tempfile::tempdir().unwrap();
        let slot = CaptureSlot::claim(parent.path(), "region.png").unwrap();
        std::fs::write(slot.path(), b"png-bytes").unwrap();
        assert_eq!(slot.written_bytes(), Some(9));
    }

    #[test]
    fn a_finished_capture_still_deletes_itself() {
        // The privacy guarantee: an image of someone's screen never outlives
        // the value that owns it, on any path.
        let parent = tempfile::tempdir().unwrap();
        let path = {
            let slot = CaptureSlot::claim(parent.path(), "region.png").unwrap();
            std::fs::write(slot.path(), b"png-bytes").unwrap();
            let image = slot.finish();
            assert!(image.path().exists());
            image.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn a_panic_mid_capture_still_deletes_the_image() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().to_path_buf();
        let result = std::panic::catch_unwind(move || {
            let slot = CaptureSlot::claim(&root, "region.png").unwrap();
            std::fs::write(slot.path(), b"png-bytes").unwrap();
            panic!("recognition blew up");
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(parent.path()).unwrap().count(),
            0,
            "the staging directory must be empty after a panic"
        );
    }

    // --- PNG header --------------------------------------------------------

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut header = PNG_SIGNATURE.to_vec();
        header.extend_from_slice(&13u32.to_be_bytes());
        header.extend_from_slice(b"IHDR");
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header
    }

    #[test]
    fn png_dimensions_read_the_ihdr() {
        assert_eq!(png_dimensions(&png_header(800, 400)), Some((800, 400)));
    }

    #[test]
    fn png_dimensions_reject_anything_else() {
        assert_eq!(png_dimensions(&[]), None);
        assert_eq!(png_dimensions(&png_header(800, 400)[..20]), None);
        assert_eq!(
            png_dimensions(b"\x89PNG\r\n\x1a\nnope-not-an-ihdr!!!!"),
            None
        );
        assert_eq!(png_dimensions(&png_header(0, 400)), None);
    }

    // --- coordinate flip ---------------------------------------------------

    #[test]
    fn vision_boxes_flip_from_bottom_left_to_top_left() {
        // Measured: a headline drawn at the top of an 800x400 image came back
        // as x=0.050 y=0.835 w=0.402 h=0.085. Top-left space must put it near
        // y=0, not near y=400.
        let rect = NormalizedRect {
            x: 0.050,
            y: 0.835,
            width: 0.402,
            height: 0.085,
        };
        let bbox = rect.to_text_box(800.0, 400.0);
        assert!((bbox.left - 40.0).abs() < 0.01);
        assert!((bbox.right - 361.6).abs() < 0.01);
        assert!((bbox.top - 32.0).abs() < 0.01, "top was {}", bbox.top);
        assert!((bbox.bottom - 66.0).abs() < 0.01);
        assert!(bbox.height() > 0.0, "a flipped box must not be inverted");
    }

    #[test]
    fn the_flip_orders_a_low_box_below_a_high_one() {
        let high = NormalizedRect {
            x: 0.0,
            y: 0.8,
            width: 0.5,
            height: 0.1,
        }
        .to_text_box(1000.0, 500.0);
        let low = NormalizedRect {
            x: 0.0,
            y: 0.1,
            width: 0.5,
            height: 0.1,
        }
        .to_text_box(1000.0, 500.0);
        assert!(high.top < low.top);
    }

    // --- reading order -----------------------------------------------------

    /// The measured probe layout, in pixels after the flip: a short headline
    /// over two columns of two lines each, in an 800x400 image.
    fn probe_layout() -> Vec<Observation> {
        vec![
            // Deliberately shuffled: Vision's order is not document order.
            obs("Beta one", 460.0, 117.2, 574.4, 144.8),
            obs("Alpha two", 40.0, 167.6, 164.0, 198.0),
            obs("HEADLINE ACROSS", 40.0, 32.0, 361.6, 66.0),
            obs("Beta two", 460.0, 168.0, 572.0, 194.4),
            obs("Alpha one", 40.0, 116.8, 168.8, 149.2),
        ]
    }

    #[test]
    fn two_columns_are_read_down_not_across() {
        let text = observations_to_text(&probe_layout());
        assert_eq!(
            text,
            "HEADLINE ACROSS\nAlpha one\nAlpha two\nBeta one\nBeta two"
        );
    }

    #[test]
    fn two_columns_are_not_the_naive_top_then_left_order() {
        // The bug this exists to prevent: sorting by Y then X interleaves the
        // columns, because the columns' baselines line up.
        let observations = probe_layout();
        let naive = {
            let mut sorted = observations.clone();
            sorted.sort_by(|a, b| {
                a.bbox
                    .top
                    .total_cmp(&b.bbox.top)
                    .then(a.bbox.left.total_cmp(&b.bbox.left))
            });
            sorted
                .iter()
                .map(|o| o.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            naive,
            "HEADLINE ACROSS\nAlpha one\nBeta one\nAlpha two\nBeta two"
        );
        assert_ne!(observations_to_text(&observations), naive);
    }

    #[test]
    fn a_full_width_headline_is_peeled_off_before_the_columns_split() {
        // A headline that crosses the gutter makes every column cut invalid for
        // the page as a whole, so the row cut fires first. Same answer by a
        // different route, and the route is the point.
        let mut observations = probe_layout();
        for o in &mut observations {
            if o.text.starts_with("HEADLINE") {
                o.bbox.right = 760.0;
            }
        }
        assert_eq!(
            observations_to_text(&observations),
            "HEADLINE ACROSS\nAlpha one\nAlpha two\nBeta one\nBeta two"
        );
    }

    #[test]
    fn a_short_heading_is_absorbed_into_the_column_it_overhangs() {
        // Pinned limitation, not an aspiration: the heading keeps its text and
        // the columns stay unscrambled, but a heading narrower than the gutter
        // sorts with the left column rather than above both.
        let observations = probe_layout();
        let ordered = reading_order(observations);
        let texts: Vec<&str> = ordered.iter().map(|o| o.text.as_str()).collect();
        assert_eq!(texts[0], "HEADLINE ACROSS");
        assert_eq!(&texts[1..3], ["Alpha one", "Alpha two"]);
        assert_eq!(&texts[3..], ["Beta one", "Beta two"]);
    }

    #[test]
    fn a_single_column_reads_top_to_bottom() {
        let observations = vec![
            obs("third", 10.0, 90.0, 300.0, 110.0),
            obs("first", 10.0, 10.0, 300.0, 30.0),
            obs("second", 10.0, 50.0, 300.0, 70.0),
        ];
        assert_eq!(observations_to_text(&observations), "first\nsecond\nthird");
    }

    #[test]
    fn runs_sharing_a_line_join_with_a_space() {
        // Vision splits at wide gaps; a two-item line must not become two
        // lines, and the gap here is under the gutter floor so no column cut
        // fires either.
        let observations = vec![
            obs("Serial", 10.0, 10.0, 80.0, 30.0),
            obs("AB-1234", 110.0, 11.0, 220.0, 31.0),
            obs("next line", 10.0, 50.0, 220.0, 70.0),
        ];
        assert_eq!(
            observations_to_text(&observations),
            "Serial AB-1234\nnext line"
        );
    }

    #[test]
    fn a_lone_value_across_a_wide_gap_is_not_treated_as_a_column() {
        // Two observations with a huge gap are a label and a value on one
        // line, not two columns; MIN_COLUMN_MEMBERS is what keeps the line
        // whole.
        let observations = vec![
            obs("Status", 10.0, 10.0, 90.0, 30.0),
            obs("Ready", 600.0, 10.0, 700.0, 30.0),
        ];
        assert_eq!(observations_to_text(&observations), "Status Ready");
    }

    #[test]
    fn overlapping_boxes_do_not_lose_text_or_loop() {
        // A translucent badge over a paragraph: no clean cut exists anywhere,
        // so this must fall through to the leaf sort rather than recurse.
        let observations = vec![
            obs("under", 10.0, 10.0, 300.0, 60.0),
            obs("over", 20.0, 20.0, 280.0, 50.0),
            obs("also over", 30.0, 15.0, 290.0, 55.0),
        ];
        let text = observations_to_text(&observations);
        for expected in ["under", "over", "also over"] {
            assert!(
                text.contains(expected),
                "{expected:?} missing from {text:?}"
            );
        }
    }

    #[test]
    fn a_rotated_line_keeps_its_text_and_its_place() {
        // Vision reports a rotated line as one tall, narrow box. It must not
        // vanish, and it must not drag its neighbours out of order.
        let observations = vec![
            obs("body one", 100.0, 10.0, 400.0, 30.0),
            obs("SIDEBAR", 10.0, 10.0, 40.0, 300.0),
            obs("body two", 100.0, 50.0, 400.0, 70.0),
        ];
        let text = observations_to_text(&observations);
        assert!(text.contains("SIDEBAR"));
        let one = text.find("body one").unwrap();
        let two = text.find("body two").unwrap();
        assert!(one < two, "body order reversed in {text:?}");
    }

    #[test]
    fn nothing_recognised_is_an_empty_string_not_a_panic() {
        assert_eq!(observations_to_text(&[]), "");
        assert!(reading_order(Vec::new()).is_empty());
    }

    #[test]
    fn whitespace_only_observations_are_dropped() {
        let observations = vec![
            obs("   ", 10.0, 10.0, 50.0, 30.0),
            obs("real", 10.0, 50.0, 50.0, 70.0),
        ];
        assert_eq!(observations_to_text(&observations), "real");
    }

    #[test]
    fn a_tall_layout_stays_within_the_recursion_bound() {
        // 400 stacked lines: a peel-one-per-cut worst case for the recursion.
        let observations: Vec<Observation> = (0..400)
            .map(|i| {
                let top = i as f64 * 30.0;
                obs(&format!("line {i}"), 10.0, top, 300.0, top + 20.0)
            })
            .collect();
        let text = observations_to_text(&observations);
        assert!(text.starts_with("line 0\nline 1\n"));
        assert!(text.ends_with("line 399"));
    }

    // --- options -----------------------------------------------------------

    #[test]
    fn language_correction_defaults_on() {
        // Prose is the common case; the settings page is where a user working
        // on code turns it off.
        assert!(OcrOptions::default().language_correction);
        assert!(OcrOptions::default().languages.is_empty());
    }
}
