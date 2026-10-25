//! Win32 Z-order operations for tiled motion. Protected overlays keep their
//! relative order; ordinary tiles must never become topmost or take focus.

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_HWNDPREV, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, HWND_TOP,
        IsWindowVisible, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowPos,
        WS_EX_TOPMOST,
};
use windows::core::BOOL;

/// Raise tiled windows inside their band, below every visible non-topmost
/// protected window. Inserting after a topmost HWND would PROMOTE a normal
/// tile into the topmost band, so topmost overlays are deliberately not used
/// as anchors. HWND_TOP keeps normal tiles below that band automatically.
/// Input `windows` is bottom-to-top. Submit top-to-bottom, chaining each
/// lower window below its predecessor; never lift an incoming tile over the
/// destination even temporarily. Protected windows never move.
pub fn raise_below(
        windows: &[HWND],
        protected: &[HWND],
) {
        struct Anchors<'a> {
                protected: &'a [HWND],
                top_down: Vec<(isize, bool, bool)>,
        }
        unsafe extern "system" fn collect(
                hwnd: HWND,
                param: LPARAM,
        ) -> BOOL {
                let anchors = unsafe { &mut *(param.0 as *mut Anchors<'_>) };
                if anchors.protected.contains(&hwnd) {
                        anchors.top_down.push((
                                hwnd.0 as isize,
                                unsafe { IsWindowVisible(hwnd).as_bool() },
                                unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
                                        & WS_EX_TOPMOST.0
                                        != 0,
                        ));
                }
                BOOL(1)
        }
        let mut anchors = Anchors {
                protected,
                top_down: Vec::new(),
        };
        if !protected.is_empty() {
                unsafe {
                        let _ = EnumWindows(Some(collect), LPARAM(&mut anchors as *mut _ as isize));
                }
        }
        let mut insert_after = lowest_protected(&anchors.top_down)
                .map(|id| HWND(id as *mut _))
                .unwrap_or(HWND_TOP);
        for &hwnd in windows.iter().rev() {
                // Respect an application's own always-on-top setting. Never
                // use it as the next anchor (that would promote normal tiles).
                if hwnd == insert_after
                        || protected.contains(&hwnd)
                        || unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 } & WS_EX_TOPMOST.0
                                != 0
                {
                        continue;
                }
                // Most calls repair an already correct band. Avoid repeated
                // WINDOWPOS/repaint traffic, including the top normal window
                // whose predecessor may be a topmost overlay.
                let in_place = unsafe { GetWindow(hwnd, GW_HWNDPREV) }.is_ok_and(|previous| {
                        if insert_after == HWND_TOP {
                                previous.0.is_null()
                                        || unsafe {
                                                GetWindowLongPtrW(previous, GWL_EXSTYLE) as u32
                                        } & WS_EX_TOPMOST.0
                                                != 0
                        } else {
                                previous == insert_after
                        }
                });
                if !in_place {
                        let result = unsafe {
                                SetWindowPos(
                                        hwnd,
                                        Some(insert_after),
                                        0,
                                        0,
                                        0,
                                        0,
                                        SWP_NOMOVE
                                                | SWP_NOSIZE
                                                | SWP_NOACTIVATE
                                                | SWP_NOOWNERZORDER,
                                )
                        };
                        if let Err(e) = result {
                                log::debug!("tiled layer repair failed for {hwnd:?}: {e}");
                                // Stop this repair: lower siblings must not
                                // jump over a surface we failed to position.
                                return;
                        }
                }
                insert_after = hwnd;
        }
}

fn lowest_protected(top_down: &[(isize, bool, bool)]) -> Option<isize> {
        top_down.iter()
                .rev()
                .find_map(|&(id, visible, topmost)| (visible && !topmost).then_some(id))
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn tiles_insert_below_all_visible_normal_overlays_without_promoting_to_topmost() {
                // Global top-down Z order: overview, topmost float, two normal
                // floats (potentially on different outputs), hidden float.
                let overlays = [
                        (10, true, true),
                        (20, true, true),
                        (30, true, false),
                        (40, true, false),
                        (50, false, false),
                ];
                let mut anchor = lowest_protected(&overlays).unwrap();
                assert_eq!(anchor, 40);
                let mut z = vec![10, 20, 30, 40, 1, 2];
                // Desired bottom-to-top [1, 2]: place the top participant
                // first, then chain its sibling below it.
                for id in [2, 1] {
                        z.retain(|&i| i != id);
                        let at = z.iter().position(|&i| i == anchor).unwrap() + 1;
                        z.insert(at, id);
                        anchor = id;
                }
                assert_eq!(z, vec![10, 20, 30, 40, 2, 1]);
        }

        #[test]
        fn win32_layer_repair_preserves_overlay_order_and_normal_window_band() {
                use windows::Win32::UI::WindowsAndMessaging::{
                        CreateWindowExW, DestroyWindow, GW_HWNDPREV, GetWindow, HWND_TOPMOST,
                        SWP_SHOWWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
                };
                use windows::core::w;
                struct TestWindow(HWND);
                impl Drop for TestWindow {
                        fn drop(&mut self) {
                                let _ = unsafe { DestroyWindow(self.0) };
                        }
                }
                fn window() -> TestWindow {
                        // Native, screen-off tool windows only: never activate
                        // or send input to any of the user's applications.
                        TestWindow(unsafe {
                                CreateWindowExW(
                                        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                                        w!("STATIC"),
                                        w!("wini layering regression"),
                                        WS_POPUP,
                                        -32000,
                                        -32000,
                                        1,
                                        1,
                                        None,
                                        None,
                                        None,
                                        None,
                                )
                                .unwrap()
                        })
                }
                fn place(
                        hwnd: HWND,
                        after: HWND,
                ) {
                        unsafe {
                                SetWindowPos(
                                        hwnd,
                                        Some(after),
                                        0,
                                        0,
                                        0,
                                        0,
                                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                                )
                                .unwrap();
                        }
                }
                fn above(
                        a: HWND,
                        b: HWND,
                ) -> bool {
                        let mut cursor = b;
                        while let Ok(previous) = unsafe { GetWindow(cursor, GW_HWNDPREV) } {
                                if previous.0.is_null() {
                                        break;
                                }
                                if previous == a {
                                        return true;
                                }
                                cursor = previous;
                        }
                        false
                }
                let source = window();
                let moving = window();
                let float_low = window();
                let float_high = window();
                let host = window();
                place(moving.0, HWND_TOP);
                place(source.0, HWND_TOP); // Simulate focus having raised the source.
                place(float_low.0, HWND_TOP);
                place(float_high.0, HWND_TOP);
                place(host.0, HWND_TOPMOST);
                let protected = [host.0, float_high.0, float_low.0];
                for _ in 0..2 {
                        place(source.0, HWND_TOP);
                        raise_below(&[source.0, moving.0], &protected);
                        assert!(above(moving.0, source.0));
                        assert!(above(float_low.0, moving.0));
                        assert!(above(float_high.0, float_low.0));
                        assert!(above(host.0, float_high.0));
                        assert_eq!(
                                unsafe { GetWindowLongPtrW(moving.0, GWL_EXSTYLE) as u32 }
                                        & WS_EX_TOPMOST.0,
                                0,
                        );
                }
                // Stage a hidden replacement below the selected surface,
                // then reveal it with the production helper. Showing must
                // preserve the layer, geometry and foreground (no input).
                crate::win::placement::set_shown(moving.0, false);
                raise_below(&[moving.0, source.0], &protected);
                let foreground =
                        unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
                let before = crate::win::api::window_rect(moving.0);
                crate::win::placement::set_shown(moving.0, true);
                assert!(unsafe { IsWindowVisible(moving.0).as_bool() });
                assert!(above(source.0, moving.0));
                assert!(above(float_low.0, source.0));
                assert_eq!(crate::win::api::window_rect(moving.0), before);
                assert_eq!(
                        unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() },
                        foreground
                );
                // With only a topmost host, the fallback is HWND_TOP, NOT
                // the host handle (which would promote the tile to topmost).
                raise_below(&[source.0, moving.0], &[host.0]);
                assert!(above(host.0, moving.0));
                assert!(above(moving.0, source.0));
                assert_eq!(
                        unsafe { GetWindowLongPtrW(moving.0, GWL_EXSTYLE) as u32 }
                                & WS_EX_TOPMOST.0,
                        0,
                );
                // A tile with the app's own always-on-top setting is untouched.
                place(moving.0, HWND_TOPMOST);
                raise_below(&[moving.0], &protected);
                assert_ne!(
                        unsafe { GetWindowLongPtrW(moving.0, GWL_EXSTYLE) as u32 }
                                & WS_EX_TOPMOST.0,
                        0,
                );
        }

        thread_local! {
                // Observe every native layer submission, not only the final
                // state. Screen-off test windows never activate user apps.
                static WATCH: std::cell::RefCell<Option<(HWND, HWND, Vec<bool>)>> = const {
                        std::cell::RefCell::new(None)
                };
        }

        fn above(
                a: HWND,
                b: HWND,
        ) -> bool {
                use windows::Win32::UI::WindowsAndMessaging::{GW_HWNDPREV, GetWindow};
                let mut cursor = b;
                while let Ok(previous) = unsafe { GetWindow(cursor, GW_HWNDPREV) } {
                        if previous.0.is_null() {
                                break;
                        }
                        if previous == a {
                                return true;
                        }
                        cursor = previous;
                }
                false
        }

        unsafe extern "system" fn observe_order(
                hwnd: HWND,
                msg: u32,
                wp: windows::Win32::Foundation::WPARAM,
                lp: LPARAM,
        ) -> windows::Win32::Foundation::LRESULT {
                use windows::Win32::UI::WindowsAndMessaging::{
                        DefWindowProcW, WM_WINDOWPOSCHANGED,
                };
                if msg == WM_WINDOWPOSCHANGED {
                        WATCH.with(|watch| {
                                if let Some((front, back, samples)) = &mut *watch.borrow_mut() {
                                        samples.push(above(*front, *back));
                                }
                        });
                }
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }

        #[test]
        fn layer_repair_never_temporarily_lifts_incoming_over_destination() {
                use windows::Win32::UI::WindowsAndMessaging::{
                        CreateWindowExW, DestroyWindow, RegisterClassW, UnregisterClassW,
                        WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
                };
                use windows::core::w;
                let class = w!("wini layer order observation");
                struct TestWindows(Vec<HWND>);
                impl Drop for TestWindows {
                        fn drop(&mut self) {
                                WATCH.with(|watch| *watch.borrow_mut() = None);
                                for hwnd in &self.0 {
                                        let _ = unsafe { DestroyWindow(*hwnd) };
                                }
                                let _ = unsafe {
                                        UnregisterClassW(w!("wini layer order observation"), None)
                                };
                        }
                }
                assert_ne!(
                        unsafe {
                                RegisterClassW(&WNDCLASSW {
                                        lpfnWndProc: Some(observe_order),
                                        lpszClassName: class,
                                        ..Default::default()
                                })
                        },
                        0
                );
                let mut windows = TestWindows(Vec::new());
                for _ in 0..3 {
                        windows.0.push(unsafe {
                                CreateWindowExW(
                                        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                                        class,
                                        w!("wini layer test"),
                                        WS_POPUP,
                                        -32000,
                                        -32000,
                                        1,
                                        1,
                                        None,
                                        None,
                                        None,
                                        None,
                                )
                                .unwrap()
                        });
                }
                let [a, b, overlay] = windows.0[..] else {
                        unreachable!()
                };
                for hwnd in [b, a, overlay] {
                        crate::win::placement::set_shown(hwnd, true);
                        crate::win::placement::raise(hwnd);
                }
                let foreground =
                        unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
                for protected in [&[overlay][..], &[][..]] {
                        assert!(above(a, b));
                        WATCH.with(|watch| *watch.borrow_mut() = Some((a, b, Vec::new())));
                        for _ in 0..3 {
                                raise_below(&[b, a], protected);
                                assert!(above(a, b));
                        }
                        let (_, _, samples) =
                                WATCH.with(|watch| watch.borrow_mut().take().unwrap());
                        assert!(
                                samples.iter().all(|correct| *correct),
                                "A/B order flipped during native submission: {samples:?}"
                        );
                        assert_eq!(
                                unsafe {
                                        windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow(
                                        )
                                },
                                foreground
                        );
                }
        }

        #[test]
        fn topmost_or_hidden_overlays_never_become_insertion_anchors() {
                assert_eq!(lowest_protected(&[]), None);
                assert_eq!(
                        lowest_protected(&[(10, true, true), (20, false, false)]),
                        None
                );
                assert_eq!(
                        lowest_protected(&[(10, true, false), (20, true, true)]),
                        Some(10)
                );
        }
}
