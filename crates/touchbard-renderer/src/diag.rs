//! TEMPORARY frame-timing diagnostics (investigation only — removed after).
//!
//! Enabled with `TOUCHBARD_DIAG=1` (optionally `TOUCHBARD_DIAG_MS` for the
//! window length, default 4000). Records coarse pipeline events from the
//! runtime (`system.rs` `render`) and the backends (preview `handle_connection`,
//! DRM `run` loop), then prints one compact summary to stderr at the end of the
//! window and disables itself. Never prints per-frame rows.
//!
//! Events (each stamped with µs since the diag epoch):
//! - `FrameStart { animating }`  — a frame render begins (`system.frame`),
//! - `Flush   { now_ms }`        — `document.resolve(now)` finished,
//! - `Raster  { render_us }`     — Vello rasterization finished,
//! - `Wait    { wait_us }`       — backend blocked (select / poll),
//! - `SendStart`                 — backend starts presenting (ws.send / convert),
//! - `Present { present_us }`    — backend finished presenting,
//! - `Pong    { rtt_us }`        — measured WebSocket round-trip (preview only).

use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub enum Ev {
    FrameStart { animating: bool },
    Flush { now_ms: f64, resolve_us: u64 },
    SceneDone { paint_us: u64 },
    Raster { render_us: u64 },
    Wait { wait_us: u64 },
    SendStart,
    Present { present_us: u64 },
    Pong { rtt_us: u64 },
}

pub struct Diag {
    start: Instant,
    window_us: u64,
    enabled: bool,
    dumped: bool,
    events: Vec<(u64, Ev)>,
}

pub static DIAG: std::sync::Mutex<Option<Diag>> = std::sync::Mutex::new(None);

/// True once the diag is enabled (reads `TOUCHBARD_DIAG` on first use).
fn enabled() -> bool {
    let mut guard = DIAG.lock().unwrap();
    if guard.is_none() {
        let window_ms: u64 = std::env::var("TOUCHBARD_DIAG_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000);
        *guard = Some(Diag {
            start: Instant::now(),
            window_us: window_ms * 1000,
            enabled: std::env::var("TOUCHBARD_DIAG").as_deref() == Ok("1"),
            dumped: false,
            events: Vec::new(),
        });
    }
    guard.as_ref().map_or(false, |d| d.enabled)
}

/// Record one pipeline event.
pub fn record(ev: Ev) {
    if !enabled() {
        return;
    }
    let mut guard = DIAG.lock().unwrap();
    let Some(diag) = guard.as_mut() else {
        return;
    };
    if !diag.enabled || diag.dumped {
        return;
    }
    let t = diag.start.elapsed().as_micros() as u64;
    diag.events.push((t, ev));
    if t >= diag.window_us || diag.events.len() >= 8000 {
        dump_summary(diag);
        diag.dumped = true;
        diag.enabled = false;
    }
}

fn pct(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn dump_summary(diag: &mut Diag) {
    let mut raster = Vec::new(); // times of Raster events (frame cadence)
    let mut render = Vec::new(); // render_us per frame
    let mut resolve = Vec::new(); // resolve_us per frame
    let mut scene = Vec::new(); // paint_scene us per frame
    let mut waits = Vec::new(); // backend wait
    let mut presents = Vec::new(); // backend present
    let mut rtts = Vec::new(); // websocket round-trips
    let mut now_ms = Vec::new(); // animation clock at each resolve

    // Per-frame correlation CSV. One row per FrameStart→Present cycle:
    // FrameStart opens/advances a row; the next FrameStart finalizes it.
    // Column times are absolute µs (diag epoch) unless suffixed (us = duration).
    let mut frames: Vec<Vec<(u64, Ev)>> = Vec::new();
    let mut current: Vec<(u64, Ev)> = Vec::new();

    for &(t, ev) in &diag.events {
        match ev {
            Ev::Raster { render_us } => {
                raster.push(t);
                render.push(render_us);
            }
            Ev::SceneDone { paint_us } => scene.push(paint_us),
            Ev::Flush {
                now_ms: n,
                resolve_us,
            } => {
                now_ms.push(n);
                resolve.push(resolve_us);
            }
            Ev::Wait { wait_us } => waits.push(wait_us),
            Ev::Present { present_us } => presents.push(present_us),
            Ev::Pong { rtt_us } => rtts.push(rtt_us),
            Ev::FrameStart { .. } | Ev::SendStart => {}
        }
        match ev {
            Ev::FrameStart { .. } => {
                if !current.is_empty() {
                    frames.push(std::mem::take(&mut current));
                }
                current.push((t, ev));
            }
            Ev::Flush { now_ms, resolve_us } => current.push((t, Ev::Flush { now_ms, resolve_us })),
            Ev::SceneDone { paint_us } => current.push((t, Ev::SceneDone { paint_us })),
            Ev::Raster { render_us } => current.push((t, Ev::Raster { render_us })),
            Ev::SendStart => current.push((t, Ev::SendStart)),
            Ev::Present { present_us } => current.push((t, Ev::Present { present_us })),
            Ev::Wait { .. } | Ev::Pong { .. } => {}
        }
    }
    if !current.is_empty() {
        frames.push(std::mem::take(&mut current));
    }

    let mut intervals: Vec<u64> = raster
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    if !intervals.is_empty() {
        intervals.remove(0); // first interval includes startup pre-render drift
    }
    let mut now_deltas: Vec<f64> = now_ms.windows(2).map(|w| w[1] - w[0]).collect();
    if !now_deltas.is_empty() {
        now_deltas.remove(0);
    }
    let mut rsorted = intervals.clone();
    rsorted.sort_unstable();

    let div = |xs: &Vec<u64>| -> (u64, u64, u64, u64) {
        // (avg, min, max, count) using whole-us totals rounded
        if xs.is_empty() {
            return (0, 0, 0, 0);
        }
        let sum: u128 = xs.iter().map(|&v| v as u128).sum();
        let avg = (sum / xs.len() as u128) as u64;
        (
            avg,
            *xs.iter().min().unwrap(),
            *xs.iter().max().unwrap(),
            xs.len() as u64,
        )
    };
    let (r_avg, r_min, r_max, r_n) = div(&render);
    let (s_avg, s_min, s_max, _s_n) = div(&scene);
    let (w_avg, _w_min, w_max, _w_n) = div(&waits);
    let (p_avg, _p_min, p_max, _p_n) = div(&presents);
    let (v_avg, v_min, v_max, _v_n) = div(&resolve);

    let stats = |xs: &Vec<u64>| -> (u64, u64, u64, u64, u64) {
        let mut s = xs.clone();
        s.sort_unstable();
        let avg = if s.is_empty() {
            0
        } else {
            (s.iter().map(|&v| v as u128).sum::<u128>() / s.len() as u128) as u64
        };
        (
            avg,
            pct(&s, 50.0),
            pct(&s, 90.0),
            pct(&s, 95.0),
            pct(&s, 99.0),
        )
    };
    let (it_avg, it_p50, it_p90, it_p95, it_p99) = stats(&intervals);
    let (_w_avg2, _w_p50, _w_p90, w_p95, _w_p99) = stats(&waits);
    let (_p_avg2, _p_p50, _p_p90, p_p95, _p_p99) = stats(&presents);
    let now_avg = if now_deltas.is_empty() {
        0.0
    } else {
        now_deltas.iter().sum::<f64>() / now_deltas.len() as f64
    };

    let over = |ms: u64| intervals.iter().filter(|&&i| i > ms * 1000).count();

    eprintln!("--- TOUCHBARD_DIAG window={}ms ---", diag.window_us / 1000);
    eprintln!(
        "frames: {}   frame interval(us): avg={} p50={} p90={} p95={} p99={} min={} max={}",
        render.len(),
        it_avg,
        it_p50,
        it_p90,
        it_p95,
        it_p99,
        rsorted.first().copied().unwrap_or(0),
        rsorted.last().copied().unwrap_or(0),
    );
    eprintln!(
        "intervals>16.67ms: {}  >18ms: {}  >20ms: {}  >25ms: {}  >33ms: {}  <8ms: {}",
        over(16),
        over(18),
        over(20),
        over(25),
        over(33),
        intervals.iter().filter(|&&i| i < 8000).count()
    );
    eprintln!(
        "resolve(us): avg={} min={} max={}   scene paint(us): avg={} min={} max={}   total raster(us): avg={} min={} max={} (n={})",
        v_avg, v_min, v_max, s_avg, s_min, s_max, r_avg, r_min, r_max, r_n
    );
    eprintln!(
        "wait(us): avg={} p95={} max={}   present(us): avg={} p95={} max={}",
        w_avg, w_p95, w_max, p_avg, p_p95, p_max
    );
    let mut rsorted_rtt = rtts.clone();
    rsorted_rtt.sort_unstable();
    let (rt_avg, rt_min, rt_max, rt_n) = div(&rtts);
    eprintln!(
        "websocket rtt(us): avg={} min={} max={} p50={} n={}",
        rt_avg,
        rt_min,
        rt_max,
        pct(&rsorted_rtt, 50.0),
        rt_n
    );
    eprintln!(
        "animation now delta (ms): avg={:.3} n={}   (wall interval avg {:.3} ms)",
        now_avg,
        now_deltas.len(),
        it_avg as f64 / 1000.0
    );

    // Per-frame correlation CSV (times in absolute µs; *_us columns are
    // per-stage durations).
    if frames.len() >= 2 {
        eprintln!("frame_idx,frame_start_us,resolve_us,scene_paint_us,raster_us,raster_end_us,send_start_us,present_us,anim_now_ms");
        for (idx, row) in frames.iter().enumerate() {
            let mut frame_start = 0u64;
            let mut resolve = 0u64;
            let mut scene_paint = 0u64;
            let mut raster = 0u64;
            let mut raster_end = 0u64;
            let mut send_start = 0u64;
            let mut present = 0u64;
            let mut anim_now = 0.0f64;
            for &(t, ev) in row {
                match ev {
                    Ev::FrameStart { .. } => frame_start = t,
                    Ev::Flush {
                        now_ms: n,
                        resolve_us,
                    } => {
                        resolve = resolve_us;
                        anim_now = n;
                    }
                    Ev::SceneDone { paint_us } => scene_paint = paint_us,
                    Ev::Raster { render_us } => {
                        raster = render_us;
                        raster_end = t;
                    }
                    Ev::SendStart => send_start = t,
                    Ev::Present { present_us } => present = present_us,
                    Ev::Wait { .. } | Ev::Pong { .. } => {}
                }
            }
            eprintln!(
                "{idx},{frame_start},{resolve},{scene_paint},{raster},{raster_end},{send_start},{present},{anim_now:.3}"
            );
        }
    }
    eprintln!("--- end diag ---");
}
