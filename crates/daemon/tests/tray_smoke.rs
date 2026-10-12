//! 托盘冒烟测试:`spawn` 后图标必须仍注册在系统托盘,防 `_tray` 立即 drop 的回归。
//!
//! 原理:tray-icon 0.19.x 在 Windows 上为每个 `TrayIcon` 创建一个隐藏窗口
//! (固定窗口类名 `tray_icon_app`,见 `tray-icon-0.19.3/src/platform_impl/windows/mod.rs`)。
//! 只有 `TrayIcon` 被 drop 时才会 `DestroyWindow`。因此 `spawn` 返回后,
//! 若该窗口类下仍能找到窗口,即证明图标对象未被释放。
use std::time::Duration;

#[test]
fn tray_icon_survives_after_spawn() {
    // 在任何平台都应能成功启动;图标存活断言仅 Windows 可查。
    let r = daily_report::tray::spawn("http://127.0.0.1:1");
    #[cfg(windows)]
    {
        assert!(r.is_ok(), "托盘应能启动: {r:?}");
        std::thread::sleep(Duration::from_millis(500));

        // 若图标被 drop,其隐藏窗口已 DestroyWindow,FindWindowW 应找不到。
        extern "system" {
            fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        }
        let class: Vec<u16> = "tray_icon_app"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
        assert!(
            hwnd != 0,
            "spawn 后 500ms,tray_icon_app 隐藏窗口应仍存在(图标未被 drop)"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = r; // Linux/macOS 分支:仅确认 spawn 不 panic。
    }
}
