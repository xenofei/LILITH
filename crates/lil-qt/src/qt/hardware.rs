//! The Hardware panel's objects in module `Lilith.Core` (APP-127 to APP-131; P4a-R17), over
//! [`crate::series`]:
//!
//! - **`Hardware`** (a singleton): `resolutionFor(windowS)` (`"1s"` or `"1m"`),
//!   `energyPeriods(nowMs)`, the energy table's periods as JSON
//!   `[{"period": "session" | "day" | "month" | "year", "from": ms, "to": ms}]` by this PC's
//!   local calendar (the session is this process's run: it starts when the process was
//!   created), and `pid`, this process's id (the GPU users list tells this app's row by it).
//! - **`ChartSeries`**: one chart's points. Properties `windowS` (default 15 minutes; a change
//!   drops the points, and the panel loads the window's series), `widthPx` and `heightPx` (the
//!   plot's size) and `scaleMax` (the value at the plot's top; 0 for the window's peak plus a
//!   tenth); read-only `upperPaths` and `lowerPaths` (each pixel's max and min as polylines in
//!   the plot's pixels, broken at the holes in the series, at most `widthPx` points in all, for a
//!   `PathMultiline`'s `paths`), `pointCount`, `latest` and `hasLatest` (the newest value),
//!   `newestMs` and `stale` (the series stopped: its newest point is more than two steps before
//!   the window's end) and `peak` (the greatest max in the window). `load(pointsJson)` takes a
//!   `telemetry.series` metric's points; `append(tsMs, value)` a live sample, which also moves
//!   the window's end to `tsMs`, so the panel appends every sample to every chart, with NaN for
//!   a value the sample doesn't have (a lost GPU), and a stopped chart shows it; `clear()` drops
//!   the points. The downsampling and the polylines are made here, once per change (a
//!   sample a second), never per frame, and reach QML as lists of `QPointF`s: no JSON and no
//!   JavaScript point per pixel (P4a-R17).

use core::pin::Pin;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QPointF, QString, QVariant};
use lil_platform::Clock;
use lil_platform::process::{ProcessInspector, SystemInspector};
use serde_json::json;

use crate::series::{LIVE_WINDOW_S, SeriesBuffer, energy_periods, parse_points, resolution_for};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qlist.h");
        type QList_QVariant = cxx_qt_lib::QList<cxx_qt_lib::QVariant>;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(i32, pid, READ, CONSTANT)]
        type Hardware = super::HardwareRust;

        #[qinvokable]
        fn resolution_for(self: &Hardware, window_s: i32) -> QString;

        #[qinvokable]
        fn energy_periods(self: &Hardware, now_ms: f64) -> QString;

        #[qobject]
        #[qml_element]
        #[qproperty(i32, window_s)]
        #[qproperty(i32, width_px)]
        #[qproperty(f64, height_px)]
        #[qproperty(f64, scale_max)]
        #[qproperty(QList_QVariant, upper_paths, READ, NOTIFY)]
        #[qproperty(QList_QVariant, lower_paths, READ, NOTIFY)]
        #[qproperty(i32, point_count, READ, NOTIFY)]
        #[qproperty(f64, latest, READ, NOTIFY)]
        #[qproperty(bool, has_latest, READ, NOTIFY)]
        #[qproperty(f64, newest_ms, READ, NOTIFY)]
        #[qproperty(bool, stale, READ, NOTIFY)]
        #[qproperty(f64, peak, READ, NOTIFY)]
        type ChartSeries = super::ChartSeriesRust;

        /// A `telemetry.series` metric's `points`, as JSON.
        #[qinvokable]
        fn load(self: Pin<&mut ChartSeries>, points_json: &QString);

        /// A live sample at `ts_ms` (Unix ms), the window's end; `value` NaN for none.
        #[qinvokable]
        fn append(self: Pin<&mut ChartSeries>, ts_ms: f64, value: f64);

        #[qinvokable]
        fn clear(self: Pin<&mut ChartSeries>);
    }

    impl cxx_qt::Initialize for ChartSeries {}
}

pub struct HardwareRust {
    pid: i32,
}

impl Default for HardwareRust {
    fn default() -> Self {
        Self {
            pid: i32::try_from(std::process::id()).unwrap_or(-1),
        }
    }
}

/// This process's creation time in Unix ms, else the first time it was asked.
fn session_start_ms() -> i64 {
    static START: OnceLock<i64> = OnceLock::new();
    *START.get_or_init(|| {
        // A FILETIME: 100 ns ticks since 1601.
        const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
        SystemInspector
            .started(std::process::id())
            .ok()
            .and_then(|ticks| ticks.checked_sub(UNIX_EPOCH_TICKS))
            .and_then(|t| i64::try_from(t / 10_000).ok())
            .unwrap_or_else(now_ms)
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl qobject::Hardware {
    fn resolution_for(&self, window_s: i32) -> QString {
        QString::from(resolution_for(u64::try_from(window_s).unwrap_or(0)))
    }

    fn energy_periods(&self, now_ms: f64) -> QString {
        let clock = lil_platform::SystemClock::new();
        let offset = |ms: i64| {
            let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0));
            i64::from(clock.local_offset_secs(at))
        };
        let periods = energy_periods(now_ms as i64, &offset, session_start_ms());
        let rows: Vec<_> = periods
            .iter()
            .map(|(period, from, to)| json!({"period": period, "from": from, "to": to}))
            .collect();
        QString::from(&serde_json::Value::Array(rows).to_string())
    }
}

pub struct ChartSeriesRust {
    window_s: i32,
    width_px: i32,
    height_px: f64,
    scale_max: f64,
    upper_paths: QList<QVariant>,
    lower_paths: QList<QVariant>,
    point_count: i32,
    latest: f64,
    has_latest: bool,
    newest_ms: f64,
    stale: bool,
    peak: f64,
    buffer: SeriesBuffer,
}

impl Default for ChartSeriesRust {
    fn default() -> Self {
        Self {
            window_s: LIVE_WINDOW_S as i32,
            width_px: 0,
            height_px: 0.0,
            scale_max: 0.0,
            upper_paths: QList::default(),
            lower_paths: QList::default(),
            point_count: 0,
            latest: 0.0,
            has_latest: false,
            newest_ms: 0.0,
            stale: false,
            peak: 0.0,
            buffer: SeriesBuffer::new(LIVE_WINDOW_S),
        }
    }
}

impl cxx_qt::Initialize for qobject::ChartSeries {
    fn initialize(mut self: Pin<&mut Self>) {
        self.as_mut()
            .on_window_s_changed(|mut series| {
                let window = u64::try_from(series.window_s).unwrap_or(LIVE_WINDOW_S);
                series.as_mut().rust_mut().buffer.set_window(window);
                series.refresh();
            })
            .release();
        self.as_mut()
            .on_width_px_changed(qobject::ChartSeries::refresh)
            .release();
        self.as_mut()
            .on_height_px_changed(qobject::ChartSeries::refresh)
            .release();
        self.as_mut()
            .on_scale_max_changed(qobject::ChartSeries::refresh)
            .release();
    }
}

impl qobject::ChartSeries {
    fn load(mut self: Pin<&mut Self>, points_json: &QString) {
        let points = serde_json::from_str(&points_json.to_string())
            .map(|v| parse_points(&v))
            .unwrap_or_default();
        self.as_mut().rust_mut().buffer.load(points);
        self.refresh();
    }

    fn append(mut self: Pin<&mut Self>, ts_ms: f64, value: f64) {
        let mut r = self.as_mut().rust_mut();
        r.buffer.set_end(ts_ms as i64);
        r.buffer.push(ts_ms as i64, value);
        self.refresh();
    }

    fn clear(mut self: Pin<&mut Self>) {
        let window = self.buffer.window_s();
        self.as_mut().rust_mut().buffer.set_window(window);
        self.refresh();
    }

    /// Recomputes what QML reads from the buffer.
    fn refresh(mut self: Pin<&mut Self>) {
        let width = usize::try_from(self.width_px).unwrap_or(0);
        let lines = self.buffer.view(width);
        let latest = self.buffer.latest();
        let peak = self.buffer.peak();
        let height = self.height_px.max(0.0);
        let top = if self.scale_max > 0.0 {
            self.scale_max
        } else {
            (peak * 1.1).max(1.0)
        };
        let y = |v: f64| height - (v / top).clamp(0.0, 1.0) * height;
        let mut upper = QList::default();
        let mut lower = QList::default();
        let mut count = 0;
        for line in &lines {
            let mut up: QList<QVariant> = QList::default();
            let mut down: QList<QVariant> = QList::default();
            for &(x, min, max) in line {
                up.append(QVariant::from(&QPointF::new(x, y(max))));
                down.append(QVariant::from(&QPointF::new(x, y(min))));
            }
            count += line.len();
            upper.append(QVariant::from(&up));
            lower.append(QVariant::from(&down));
        }
        let newest = self.buffer.newest_ms();
        let stale = self.buffer.is_stale();
        let mut r = self.as_mut().rust_mut();
        r.upper_paths = upper;
        r.lower_paths = lower;
        r.point_count = i32::try_from(count).unwrap_or(i32::MAX);
        r.latest = latest.unwrap_or(0.0);
        r.has_latest = latest.is_some();
        r.newest_ms = newest.map_or(0.0, |ms| ms as f64);
        r.stale = stale;
        r.peak = peak;
        self.as_mut().upper_paths_changed();
        self.as_mut().lower_paths_changed();
        self.as_mut().point_count_changed();
        self.as_mut().latest_changed();
        self.as_mut().has_latest_changed();
        self.as_mut().newest_ms_changed();
        self.as_mut().stale_changed();
        self.as_mut().peak_changed();
    }
}
