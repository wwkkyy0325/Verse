//! Living in the tray, and leaving it.
//!
//! **The window is not the program.** Until this existed, closing the window
//! ended the process, so a ten-minute recording died with it: no partial
//! result, no warning, and the only way to keep a job alive was to leave a
//! window open and not touch it. The tray is what makes closing a choice.
//!
//! Everything here runs on the main thread, which constrains the one thing that
//! would otherwise be natural to write. `tauri-plugin-dialog`'s own
//! documentation is explicit that `blocking_show` "cannot be executed on the
//! main thread as it will freeze your application", so both questions below go
//! through `show` with a callback.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Window};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::bridge::App;
use crate::settings::{CloseAction, Settings};

/// What the window asks the first time ✕ is pressed.
const CLOSE_QUESTION: &str = "关闭窗口时应该怎么做？\n\n\
     这个选择只问这一次，以后想在两者之间换，删掉设置文件就会重新问。";

const HIDE_LABEL: &str = "隐藏到托盘";
const QUIT_LABEL: &str = "退出程序";

/// Put the icon in the notification area, with a menu on it.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    // Both directions, and both always present rather than one item whose text
    // depends on the window's state. A menu item whose label changes under the
    // cursor is worse than one that sometimes does nothing: Tauri has no
    // "menu about to open" event to update it at, so the text would lag the
    // truth by however long ago the window was last moved.
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &hide, &quit])?;

    let mut builder = TrayIconBuilder::with_id("verse")
        .menu(&menu)
        // Left click shows the window instead of opening the menu, because
        // that is the gesture people try first and it is the harmless one.
        .show_menu_on_left_click(false)
        .tooltip("Verse");

    // The bundle's own icon, so there is no second asset to keep in step with
    // `tools/make-icon.mjs`.
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => reveal(app),
            "hide" => tuck(app),
            "quit" => leave(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Show the window, or put it away again.
///
/// A toggle rather than "always show", and that is what most tray applications
/// do: the icon is how you reach the window, and the second press is how you
/// get it out of the way without quitting. **It is also the only deliberate way
/// to hide.** ✕ does it too, once the first-time question has been answered —
/// but ✕ reads as "close", and a person looking for "put this away for now"
/// looks at the tray, not at the button that usually means goodbye.
fn toggle(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    if window.is_visible().unwrap_or(false) {
        tuck(app);
        return;
    }

    reveal(app);
}

/// Put the window away without ending the program.
///
/// The other half of [`toggle`], and what the tray menu's 隐藏窗口 calls. It
/// does not touch the close preference: pressing ✕ was answered once and this
/// is a different gesture, so it changes nothing about what ✕ will do next.
fn tuck(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// Bring the window back, wherever it went.
fn reveal(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// What ✕ means, once the user has said.
///
/// `prevent_close` is called before anything else, in every branch. The window
/// must not disappear while the question about it is still on screen — and the
/// question is asked precisely because there is no answer yet.
pub fn close_requested(window: &Window, api: &tauri::CloseRequestApi) {
    api.prevent_close();

    let app = window.app_handle().clone();

    match Settings::load(&crate::settings_path()).close {
        Some(CloseAction::Hide) => {
            let _ = window.hide();
        }
        Some(CloseAction::Quit) => leave(&app),
        None => ask_about_closing(&app),
    }
}

/// Ask what ✕ should do, remember the answer, and do it.
fn ask_about_closing(app: &AppHandle) {
    let handle = app.clone();

    app.dialog()
        .message(CLOSE_QUESTION)
        .title("Verse")
        .buttons(MessageDialogButtons::OkCancelCustom(
            HIDE_LABEL.into(),
            QUIT_LABEL.into(),
        ))
        .show(move |hide| {
            let answer = if hide {
                CloseAction::Hide
            } else {
                CloseAction::Quit
            };

            let _ = Settings::answered(answer).save(&crate::settings_path());

            match answer {
                CloseAction::Hide => {
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.hide();
                    }
                }
                CloseAction::Quit => leave(&handle),
            }
        });
}

/// End the process, asking first when there is work outstanding.
///
/// The one exit path: the tray's 退出 and the `Quit` answer both come here, so
/// they cannot drift into two different behaviours.
///
/// It does **not** cancel the running job and wait. Cancellation is cooperative
/// and would take as long as the current span; the resume log is already flushed
/// per span, so waiting would buy nothing and would make the button look broken.
pub fn leave(app: &AppHandle) {
    let outstanding = outstanding(app);

    if outstanding == 0 {
        app.exit(0);
        return;
    }

    let handle = app.clone();
    app.dialog()
        .message(quit_question(outstanding))
        .title("Verse")
        .buttons(MessageDialogButtons::OkCancelCustom(
            QUIT_LABEL.into(),
            "取消".into(),
        ))
        .show(move |go| {
            if go {
                handle.exit(0);
            }
        });
}

/// How many files are being worked on or waiting.
///
/// Read from `running`, not from `job`, for the reason `AppState::pending`
/// gives: `job` is empty until the pipeline announces itself, so a quit landing
/// in that gap would be told there was nothing to lose.
fn outstanding(app: &AppHandle) -> usize {
    let shared = app.state::<App>();
    let state = shared.state.lock().expect("state mutex poisoned");
    state.pending()
}

/// What to say when somebody leaves with work outstanding.
///
/// A function rather than an inline `format!`, for the reason `refusal_for` is
/// one: the wording is part of the behaviour and gets to be tested. There is no
/// zero branch — the caller does not ask the question for nothing.
fn quit_question(outstanding: usize) -> String {
    format!(
        "还有 {outstanding} 个文件没有处理完。\n\n\
         现在退出会中断它们。已经识别过的部分会保留下来 —— \
         下次启动时它们还在列表里，可以接着跑，不用从头再来。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_says_what_is_lost_and_what_is_not() {
        let one = quit_question(1);
        assert!(one.contains('1'), "the count has to be in it: {one}");
        assert!(
            one.contains("保留"),
            "a dialog that does not say the work is kept reads as 'you will lose it': {one}"
        );
        assert!(
            one.contains("下次启动"),
            "the promise is only worth making if it says where to find it: {one}"
        );
    }

    #[test]
    fn the_count_is_the_one_it_was_given() {
        // Plural is not the issue — Chinese does not inflect — but a sentence
        // that quietly hard-codes a number would be, and this is how that is
        // noticed.
        assert!(quit_question(7).contains('7'));
    }
}
