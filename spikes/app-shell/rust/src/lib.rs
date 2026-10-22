//! spike_shell_bridge: the cxx-qt side of the app-shell spike (plan 4a, Task 0).
//!
//! - `GridModel`: a 10,000-row `QAbstractListModel` that exposes its rows 200 at a time through
//!   `canFetchMore` / `fetchMore` (S0.6's scrolled list).
//! - `JobsFeed`: 8 fake jobs at 10 Hz (a 512x512 JPEG frame and a progress value per tick) and 6
//!   telemetry series at 1 Hz, produced on a background thread and delivered on the GUI thread.
//! - `SpikeCounter`: a counter for the Narrator check (S0.7).

pub mod counter;
pub mod feed;
pub mod grid;
