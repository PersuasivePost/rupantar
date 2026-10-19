//! Where to put the radial window so it is centered on the cursor on any monitor layout.
//!
//! Pure arithmetic with no Tauri types, so it is tested on any machine. All inputs and the window
//! position are in *physical* desktop pixels (the coordinate space the OS reports for the cursor
//! and monitors); only `cursor_x/cursor_y` in the result are logical (CSS) pixels.

/// A monitor in physical desktop pixels. `x`/`y` can be negative (monitor left of / above primary).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel (1.0, 1.25, 2.0, ...).
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Top-left of the window, physical desktop pixels.
    pub x: i32,
    pub y: i32,
    /// Window width and height (it is square), physical pixels, at the target monitor's scale.
    pub size: u32,
    /// Cursor position inside the window in logical pixels, i.e. where the menu's center goes.
    pub cursor_x: f64,
    pub cursor_y: f64,
    pub scale: f64,
}

/// The monitor containing the point, else the nearest one.
fn pick_monitor(cx: f64, cy: f64, monitors: &[MonitorRect]) -> Option<&MonitorRect> {
    let dist2 = |m: &MonitorRect| {
        let (l, t) = (f64::from(m.x), f64::from(m.y));
        let (r, b) = (l + f64::from(m.width), t + f64::from(m.height));
        // Zero when inside; otherwise the gap to the closest edge on each axis.
        let dx = (l - cx).max(0.0).max(cx - r + 1.0);
        let dy = (t - cy).max(0.0).max(cy - b + 1.0);
        dx * dx + dy * dy
    };
    monitors.iter().min_by(|a, b| dist2(a).total_cmp(&dist2(b)))
}

/// Centers a square window of `logical_size` on the cursor, then slides it fully onto the monitor
/// under the cursor so the menu is never cut off at a screen edge.
pub fn place(cursor_x: f64, cursor_y: f64, monitors: &[MonitorRect], logical_size: f64) -> Placement {
    let monitor = pick_monitor(cursor_x, cursor_y, monitors);
    let scale = monitor.map_or(1.0, |m| if m.scale > 0.0 { m.scale } else { 1.0 });
    let size = (logical_size * scale).round().max(1.0);

    let fit = |center: f64, origin: i32, extent: u32| {
        let mut pos = (center - size / 2.0).round();
        if monitor.is_some() {
            let lo = f64::from(origin);
            let hi = lo + f64::from(extent) - size;
            pos = if hi < lo { lo } else { pos.clamp(lo, hi) };
        }
        pos as i32
    };
    let (x, y) = match monitor {
        Some(m) => (fit(cursor_x, m.x, m.width), fit(cursor_y, m.y, m.height)),
        None => (fit(cursor_x, 0, 0), fit(cursor_y, 0, 0)),
    };

    Placement {
        x,
        y,
        size: size as u32,
        cursor_x: (cursor_x - f64::from(x)) / scale,
        cursor_y: (cursor_y - f64::from(y)) / scale,
        scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mon(x: i32, y: i32, width: u32, height: u32, scale: f64) -> MonitorRect {
        MonitorRect { x, y, width, height, scale }
    }

    #[test]
    fn centers_on_cursor_in_the_middle_of_a_screen() {
        let p = place(960.0, 540.0, &[mon(0, 0, 1920, 1080, 1.0)], 600.0);
        assert_eq!((p.x, p.y, p.size), (660, 240, 600));
        assert_eq!((p.cursor_x, p.cursor_y), (300.0, 300.0));
    }

    #[test]
    fn slides_onto_the_screen_at_every_edge_and_reports_the_real_cursor_offset() {
        let m = [mon(0, 0, 1920, 1080, 1.0)];
        let tl = place(10.0, 10.0, &m, 600.0);
        assert_eq!((tl.x, tl.y), (0, 0));
        assert_eq!((tl.cursor_x, tl.cursor_y), (10.0, 10.0)); // menu center stays under the cursor
        let br = place(1915.0, 1075.0, &m, 600.0);
        assert_eq!((br.x, br.y), (1320, 480));
        assert_eq!((br.cursor_x, br.cursor_y), (595.0, 595.0));
    }

    #[test]
    fn monitor_left_of_primary_has_negative_coordinates() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(-1280, 0, 1280, 1024, 1.0)];
        // 100px from the secondary's right edge: slides left so it never straddles into the primary.
        let p = place(-100.0, 500.0, &m, 600.0);
        assert_eq!((p.x, p.y), (-600, 200));
        assert_eq!(p.cursor_x, 500.0);
        let edge = place(-1275.0, 500.0, &m, 600.0);
        assert_eq!(edge.x, -1280); // clamped to the secondary's left edge, not the primary's
        assert_eq!(edge.cursor_x, 5.0);
    }

    #[test]
    fn mixed_dpi_uses_the_scale_of_the_monitor_under_the_cursor() {
        // 4K at 200% on the left, 1080p at 100% on the right.
        let m = [mon(0, 0, 3840, 2160, 2.0), mon(3840, 0, 1920, 1080, 1.0)];
        let hi = place(100.0, 100.0, &m, 600.0);
        assert_eq!((hi.x, hi.y, hi.size, hi.scale), (0, 0, 1200, 2.0));
        assert_eq!((hi.cursor_x, hi.cursor_y), (50.0, 50.0)); // logical, not physical
        let lo = place(4800.0, 100.0, &m, 600.0);
        assert_eq!((lo.x, lo.y, lo.size, lo.scale), (4500, 0, 600, 1.0));
        assert_eq!((lo.cursor_x, lo.cursor_y), (300.0, 100.0));
    }

    #[test]
    fn fractional_scale_rounds_the_window_size() {
        let p = place(500.0, 500.0, &[mon(0, 0, 2560, 1440, 1.25)], 600.0);
        assert_eq!(p.size, 750);
        assert_eq!((p.x, p.y), (125, 125));
        assert_eq!((p.cursor_x, p.cursor_y), (300.0, 300.0));
    }

    #[test]
    fn cursor_in_a_gap_between_monitors_uses_the_nearest() {
        // 100px gap between a 1080p and a 4K screen.
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(2020, 0, 3840, 2160, 2.0)];
        assert_eq!(place(1950.0, 500.0, &m, 600.0).scale, 1.0);
        assert_eq!(place(2000.0, 500.0, &m, 600.0).scale, 2.0);
    }

    #[test]
    fn monitor_edges_are_half_open() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, 0, 1920, 1080, 2.0)];
        assert_eq!(place(1919.0, 10.0, &m, 600.0).scale, 1.0);
        assert_eq!(place(1920.0, 10.0, &m, 600.0).scale, 2.0);
    }

    #[test]
    fn window_larger_than_the_monitor_pins_to_its_origin() {
        let p = place(100.0, 100.0, &[mon(0, 0, 400, 300, 1.0)], 600.0);
        assert_eq!((p.x, p.y), (0, 0));
    }

    #[test]
    fn no_monitor_info_falls_back_to_unclamped_scale_one() {
        let p = place(100.0, 100.0, &[], 600.0);
        assert_eq!((p.x, p.y, p.size, p.scale), (-200, -200, 600, 1.0));
        assert_eq!((p.cursor_x, p.cursor_y), (300.0, 300.0));
    }

    #[test]
    fn bogus_scale_is_treated_as_one() {
        let p = place(500.0, 500.0, &[mon(0, 0, 1920, 1080, 0.0)], 600.0);
        assert_eq!((p.size, p.scale), (600, 1.0));
    }
}
