//! **Port-only: frame pacing.** The game runs at 60 Hz (one logic tick per NTSC field), and nothing in it is written
//! for another rate, so the tick stays 60 Hz at every frame rate: physics, animation and timers run at 1.0x whatever
//! the frame rate is. At the game's own rate (the default, `FrameRate::Fps60`) the port draws no frame between two
//! ticks, so a frame drawn without a tick is the previous picture again, and a frame that runs two ticks skips one:
//! either shows as a stutter. This module keeps one tick per drawn frame and draws at most 60 frames a second.
//!
//! **Frame rate option** (crate::graphics `FrameRate`: 60, 120, 180, 240, Uncapped; Port Options, `RC_FPS`): above 60
//! the frames come from the clock and the ticks from the frames. Each frame runs the whole ticks that have passed since
//! the last one (the plain floor, the remainder carried), usually none or one, and crate::interp draws the frame from
//! the poses between the last two ticks, `alpha` = that remainder. The limiter then waits for 1/N s (Uncapped: not at
//! all), and the window presents without vsync (a vsync that is not the rate asked for would beat against it); at 60
//! the display's own rhythm paces as below. A stall of more than [`MAX_STEPS`] ticks is dropped in every mode.
//!
//! * **Ticks** (`First`, before Bevy's time update): each frame advances `TimeUpdateStrategy::FixedTimesteps(n)`
//!   ticks, `n` from the real time since the last frame start counted in ticks with a remainder that carries over:
//!   while the limiter runs, `n` rounds (a frame a little early or late is still one tick; one that arrives
//!   more than half a tick early still gets its tick, the remainder kept within ±½), so the usual frame is exactly one
//!   tick and only a real hiccup catches up; without the limiter `n` is the plain floor of the elapsed ticks. At
//!   most [`MAX_STEPS`] a frame: time lost in a longer stall (a level load) is dropped.
//! * **Limiter** (`Last`): the frame waits for its deadline. The period follows the primary monitor: at ~60 Hz none
//!   (vsync paces the loop; a second clock beside it would beat against the display and drop a frame now and then);
//!   at a multiple of ~60 Hz (120 Hz ProMotion, 240 Hz) that many refreshes, so the frames stay on the display's own
//!   rhythm; otherwise 1/60 s (a 144 Hz screen without variable refresh still shows some frames longer than others).
//!   The wait sleeps to 0.3 ms before the deadline, then yields until it (the yield loop costs CPU: keep it short).
//!   At ~60 Hz the display's vsync is trusted only while it paces the loop: when the frames come measurably faster
//!   than 60 a second (vsync forced off, a variable-refresh or captured display), the 1/60 s limiter takes over for
//!   the rest of the session. Unpaced, the tick rule above runs a frame that is more than ¼ tick early (and the
//!   remainder clamp drops the debt), so the whole game (movement, music, speech) ran up to a quarter fast.
//!
//! `RC_FRAME_CAP=0` (or `RC_NOVSYNC=1`, which measures headroom) turns the limiter off; frame-exact runs
//! (`crate::determinism`) keep their own one tick per update, without the limiter unless `RC_FRAME_CAP=1`.

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use bevy::window::{Monitor, PresentMode, PrimaryMonitor, PrimaryWindow};
use crate::graphics::{FrameRate, GraphicsSettings};
use std::time::{Duration, Instant};

/// Ticks a frame may run at most (a longer stall's time is dropped).
pub const MAX_STEPS: u32 = 4;
/// The tick period (1/60 s).
const TICK: f64 = 1.0 / crate::determinism::TICK_HZ;

pub struct FramePacePlugin;

#[derive(Resource)]
struct Pace {
    limit: bool,
    /// The last frame's start (the ticks' clock).
    last: Option<Instant>,
    /// Elapsed ticks not yet run.
    acc: f64,
    /// The last frame was one of the blended modes (frame rate above 60).
    blended: bool,
    /// The limiter's next deadline and its period.
    next: Option<Instant>,
    period: Option<Duration>,
    /// The frame interval's moving average (seconds; the ticks' clock).
    avg: f64,
    /// A ~60 Hz display whose vsync does not pace the loop: the 1/60 s limiter runs anyway (module docs).
    unpaced: bool,
}

fn env_is(name: &str, v: &str) -> bool { std::env::var(name).is_ok_and(|x| x.trim() == v) }

impl Plugin for FramePacePlugin {
    fn build(&self, app: &mut App) {
        if crate::determinism::deterministic() {
            // Frame-exact runs keep their one tick per update; `RC_FRAME_CAP=1` still limits them to 60 frames a second
            // (a capture run otherwise renders as fast as the machine allows).
            if env_is("RC_FRAME_CAP", "1") {
                app.insert_resource(Pace { limit: true, last: None, acc: 0.0, blended: false, next: None, period: None, avg: TICK, unpaced: false }).add_systems(Last, wait);
            }
            return;
        }
        let limit = !env_is("RC_FRAME_CAP", "0") && !env_is("RC_NOVSYNC", "1");
        app.insert_resource(Pace { limit, last: None, acc: 0.0, blended: false, next: None, period: None, avg: TICK, unpaced: false })
            .insert_resource(TimeUpdateStrategy::FixedTimesteps(1))
            .add_systems(First, steps.before(bevy::time::TimeSystems))
            .add_systems(Update, present_mode);
        if limit { app.add_systems(Last, wait); }
    }
}

/// One frame's ticks and the remainder carried over (module docs): `elapsed` ticks of real time have passed since the
/// last frame start. `rounding` (the limiter at the game's own rate): a frame a little early or late is still one tick,
/// the remainder within ±½. Else the plain floor, the remainder in 0..1: the part of a tick the frame is past the last
/// one that ran.
fn advance(acc: f64, elapsed: f64, rounding: bool) -> (u32, f64) {
    let acc = acc + elapsed;
    let n = if rounding {
        let n = acc.round().max(0.0);
        if n == 0.0 && acc > 0.25 { 1.0 } else { n }
    } else {
        acc.floor().max(0.0)
    };
    let n = n.min(MAX_STEPS as f64);
    let rest = acc - n;
    (n as u32, if rounding { rest.clamp(-0.5, 0.5) } else { rest.clamp(0.0, 1.0) })
}

/// This frame's ticks (module docs), and what crate::interp needs to draw it.
fn steps(mut pace: ResMut<Pace>, mut strategy: ResMut<TimeUpdateStrategy>, gfx: Option<Res<GraphicsSettings>>) {
    let blended = gfx.is_some_and(|g| g.frame_rate.interpolates());
    let now = Instant::now();
    let Some(last) = pace.last.replace(now) else {
        *strategy = TimeUpdateStrategy::FixedTimesteps(1);
        crate::interp::begin_frame(1, 1.0, false);
        return;
    };
    // The two modes keep the remainder in different ranges: start the other one from none.
    if pace.blended != blended {
        pace.blended = blended;
        pace.acc = 0.0;
    }
    let (n, rest) = advance(pace.acc, (now - last).as_secs_f64() / TICK, pace.limit && !blended);
    pace.acc = rest;
    *strategy = TimeUpdateStrategy::FixedTimesteps(n);
    // At the game's own rate every frame shows the last tick as it is.
    crate::interp::begin_frame(n, if blended { rest as f32 } else { 1.0 }, blended);
}

/// The window's present mode follows the frame rate: vsync at the game's own 60, none above (the limiter or the
/// machine sets the rate; `RC_NOVSYNC=1` presents without vsync at 60 too).
fn present_mode(gfx: Option<Res<GraphicsSettings>>, mut windows: Query<&mut Window, With<PrimaryWindow>>, mut applied: Local<Option<PresentMode>>) {
    let rate = gfx.map_or(FrameRate::Fps60, |g| g.frame_rate);
    let want = if rate.interpolates() || env_is("RC_NOVSYNC", "1") { PresentMode::AutoNoVsync } else { PresentMode::AutoVsync };
    if *applied == Some(want) || windows.is_empty() { return; }
    for mut w in &mut windows {
        if w.present_mode != want { w.present_mode = want; }
    }
    *applied = Some(want);
}

/// The limiter's period for a monitor of `mhz` millihertz (module docs): None at ~60 Hz.
fn period_for(mhz: Option<u32>) -> Option<Duration> {
    let hz = mhz.map(|m| m as f64 / 1000.0).filter(|&h| h > 1.0);
    let Some(hz) = hz else { return Some(Duration::from_secs_f64(TICK)) };
    let k = (hz / 60.0).round();
    if k >= 1.0 && (hz / k - 60.0).abs() < 1.5 {
        if k == 1.0 { return None; }
        return Some(Duration::from_secs_f64(k / hz));
    }
    Some(Duration::from_secs_f64(TICK))
}

/// The limiter (module docs).
fn wait(mut pace: ResMut<Pace>, monitors: Query<&Monitor, With<PrimaryMonitor>>, gfx: Option<Res<GraphicsSettings>>) {
    let period = match gfx.map_or(FrameRate::Fps60, |g| g.frame_rate) {
        FrameRate::Fps60 => period_for(monitors.iter().next().and_then(|m| m.refresh_rate_millihertz)),
        rate => rate.cap_hz().map(|hz| Duration::from_secs_f64(1.0 / hz)),
    };
    if period != pace.period {
        pace.period = period;
        pace.next = None;
    }
    let Some(period) = period else { return };
    let now = Instant::now();
    // A deadline more than a period behind (a stall) starts the rhythm again from now.
    let deadline = match pace.next {
        Some(d) if d + period > now => d,
        _ => now,
    };
    let spin = Duration::from_micros(300);
    if deadline > now + spin { std::thread::sleep(deadline - now - spin); }
    while Instant::now() < deadline { std::thread::yield_now(); }
    pace.next = Some(deadline + period);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs frames of `dt` ticks each; the ticks run and the last remainder.
    fn run(frames: usize, dt: f64, rounding: bool) -> (u32, f64) {
        let (mut acc, mut ticks) = (0.0, 0);
        for _ in 0..frames {
            let (n, rest) = advance(acc, dt, rounding);
            (acc, ticks) = (rest, ticks + n);
        }
        (ticks, acc)
    }

    #[test]
    fn the_tick_keeps_its_speed_at_every_frame_rate() {
        // One second of frames at 60, 120, 180, 240 and 1000 a second: 60 ticks (±1 for the remainder), in the floor
        // mode of the blended rates.
        for fps in [60.0, 120.0, 180.0, 240.0, 1000.0] {
            let (ticks, rest) = run(fps as usize, 60.0 / fps, false);
            assert!((59..=60).contains(&ticks), "{fps} fps: {ticks} ticks");
            assert!((0.0..1.0).contains(&rest));
            assert!((ticks as f64 + rest - 60.0).abs() < 1e-6, "{fps} fps: no time is lost");
        }
    }

    #[test]
    fn frames_between_ticks_carry_the_remainder() {
        // 240 fps: three frames without a tick, the fourth runs it; the remainder is the blend factor.
        let mut acc = 0.0;
        for k in 1..=4 {
            let (n, rest) = advance(acc, 0.25, false);
            assert_eq!(n, (k == 4) as u32);
            assert!((rest - if k == 4 { 0.0 } else { 0.25 * k as f64 }).abs() < 1e-9);
            acc = rest;
        }
        // A frame 1.5 ticks long runs one and keeps the half.
        assert_eq!(advance(0.0, 1.5, false), (1, 0.5));
    }

    #[test]
    fn a_stall_drops_its_time() {
        assert_eq!(advance(0.0, 30.0, false), (MAX_STEPS, 1.0));
        assert_eq!(advance(0.0, 30.0, true), (MAX_STEPS, 0.5));
    }

    #[test]
    fn the_game_rate_still_rounds() {
        // At 60 with the limiter a frame a little early or late is one tick.
        assert_eq!(advance(0.0, 0.8, true).0, 1);
        assert_eq!(advance(0.0, 1.2, true).0, 1);
        assert_eq!(advance(0.0, 0.3, true).0, 1);
        assert_eq!(advance(0.0, 0.2, true).0, 0);
    }

    #[test]
    fn the_period_follows_the_display() {
        assert_eq!(period_for(Some(60_000)), None);
        assert_eq!(period_for(Some(59_940)), None);
        let p = |mhz| period_for(Some(mhz)).unwrap().as_secs_f64();
        assert!((p(120_000) - 2.0 / 120.0).abs() < 1e-9);
        assert!((p(119_880) - 2.0 / 119.88).abs() < 1e-9);
        assert!((p(240_000) - 4.0 / 240.0).abs() < 1e-9);
        assert!((p(144_000) - TICK).abs() < 1e-9);
        assert!((period_for(None).unwrap().as_secs_f64() - TICK).abs() < 1e-9);
    }
}
