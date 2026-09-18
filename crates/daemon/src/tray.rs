//! 系统托盘：蓝色圆点图标 + "打开 UI" / "退出" 菜单。

use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

/// 生成 32×32 RGBA 蓝色圆点图标数据。
pub fn icon_rgba() -> (Vec<u8>, u32, u32) {
    let size: u32 = 32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx: f64 = 16.0;
    let cy: f64 = 16.0;
    let r: f64 = 13.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let idx = ((y * size + x) * 4) as usize;
            if dx * dx + dy * dy <= r * r {
                // #2B6CB0 蓝色
                rgba[idx] = 0x2B;
                rgba[idx + 1] = 0x6C;
                rgba[idx + 2] = 0xB0;
                rgba[idx + 3] = 0xFF;
            }
        }
    }
    (rgba, size, size)
}

/// 启动托盘图标（独立线程轮询菜单事件）。
///
/// 菜单：
/// - 打开 UI → `webbrowser::open(url)`
/// - 退出 → `std::process::exit(0)`
pub fn spawn(url: &str) -> anyhow::Result<()> {
    use tray_icon::menu::MenuId;

    let (rgba, w, h) = icon_rgba();
    let icon = Icon::from_rgba(rgba, w, h).map_err(|e| anyhow::anyhow!("icon: {e}"))?;

    let menu = Menu::new();
    let open_item = MenuItem::new("打开 UI", true, None);
    let quit_item = MenuItem::new("退出", true, None);
    menu.append(&open_item)?;
    menu.append(&quit_item)?;

    // MenuItem 包含 Rc（非 Send），先提取 ID 再传入线程。
    let open_id: MenuId = open_item.id().clone();
    let quit_id: MenuId = quit_item.id().clone();

    let _tray: TrayIcon = TrayIconBuilder::new()
        .with_tooltip("daily-report")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .map_err(|e| anyhow::anyhow!("tray: {e}"))?;

    let url = url.to_string();
    std::thread::spawn(move || {
        let receiver = MenuEvent::receiver();
        loop {
            if let Ok(event) = receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                if event.id == open_id {
                    let _ = webbrowser::open(&url);
                } else if event.id == quit_id {
                    std::process::exit(0);
                }
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_rgba_is_32x32_valid() {
        let (rgba, w, h) = icon_rgba();
        assert_eq!((w, h), (32, 32));
        assert_eq!(rgba.len(), 32 * 32 * 4);
        // 中心像素应为蓝色
        let cx = (16 * 32 + 16) * 4;
        assert_eq!(rgba[cx], 0x2B);
        assert_eq!(rgba[cx + 1], 0x6C);
        assert_eq!(rgba[cx + 2], 0xB0);
        assert_eq!(rgba[cx + 3], 0xFF);
        // 角落像素应为透明
        assert_eq!(rgba[3], 0x00);
    }
}
