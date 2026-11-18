//! Dictation (Win+H) in shell mode: Windows' own voice typing is part of
//! Explorer's text-input host. Press Win+H and talk; what you say is typed
//! into the app in front as Windows hears it, until Win+H again (or a long
//! silence). It uses Windows' speech recognition, which needs a microphone
//! and Windows' "Online speech recognition" turned on (Settings > Privacy &
//! access in Cascade).
//!
//! Windows' voice typing also takes voice commands ("delete that") and
//! adds punctuation by itself; this types what it hears, as it hears it.

use std::sync::Mutex;

use windows::core::{Interface, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::SpeechRecognition::*;

use crate::{config, msg};

static RECOGNIZER: Mutex<Option<SpeechRecognizer>> = Mutex::new(None);

fn recognizer() -> std::sync::MutexGuard<'static, Option<SpeechRecognizer>> {
    RECOGNIZER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts listening, or stops if it is.
pub fn toggle() {
    if let Some(r) = recognizer().take() {
        std::thread::spawn(move || {
            let _ = r.ContinuousRecognitionSession().and_then(|s| s.StopAsync()).and_then(|a| a.get());
            msg::note("Stopped listening");
        });
        return;
    }
    std::thread::spawn(|| {
        unsafe {
            let _ =
                windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        match start() {
            Ok(r) => {
                *recognizer() = Some(r);
                msg::note("Listening\u{2026} (Win+H to stop)");
            }
            Err(why) => {
                config::log(&format!("dictation: couldn't start ({why})"));
                msg::note(why);
            }
        }
    });
}

fn start() -> Result<SpeechRecognizer, String> {
    let r = SpeechRecognizer::new().map_err(|_| "No speech recognition on this PC".to_string())?;
    let topic =
        SpeechRecognitionTopicConstraint::Create(SpeechRecognitionScenario::Dictation, &HSTRING::from("dictation"))
            .map_err(|e| e.to_string())?;
    let topic: ISpeechRecognitionConstraint = topic.cast().map_err(|e| e.to_string())?;
    r.Constraints().and_then(|c| c.Append(&topic)).map_err(|e| e.to_string())?;
    let compiled = r.CompileConstraintsAsync().and_then(|op| op.get()).map_err(|e| e.to_string())?;
    let status = compiled.Status().map_err(|e| e.to_string())?;
    if status != SpeechRecognitionResultStatus::Success {
        return Err(match status {
            SpeechRecognitionResultStatus::NetworkFailure => "Dictation needs the internet".into(),
            SpeechRecognitionResultStatus::TopicLanguageNotSupported => {
                "Dictation isn't available in your language".into()
            }
            _ => "Turn on Online speech recognition (Cascade's Settings > Privacy & access)".into(),
        });
    }
    let session = r.ContinuousRecognitionSession().map_err(|e| e.to_string())?;
    session
        .ResultGenerated(&TypedEventHandler::<
            SpeechContinuousRecognitionSession,
            SpeechContinuousRecognitionResultGeneratedEventArgs,
        >::new(|_, args| {
            if let Some(a) = args.as_ref() {
                let text = a.Result()?.Text()?.to_string();
                if !text.trim().is_empty() {
                    crate::emoji::type_text(&format!("{} ", text.trim()));
                }
            }
            Ok(())
        }))
        .map_err(|e| e.to_string())?;
    // It stops by itself after a long silence: say so.
    let _ = session.Completed(&TypedEventHandler::new(|_, _| {
        recognizer().take();
        msg::note("Stopped listening");
        Ok(())
    }));
    session.StartAsync().and_then(|a| a.get()).map_err(|e| {
        if e.code().0 as u32 == 0x8004_5509 {
            "Turn on Online speech recognition (Cascade's Settings > Privacy & access)".to_string()
        } else {
            format!("Dictation couldn't start ({e})")
        }
    })?;
    Ok(r)
}

/// Windows' "Online speech recognition" (needed for dictation).
pub fn online_allowed() -> bool {
    crate::winset::reg_dword(
        windows::Win32::System::Registry::HKEY_CURRENT_USER,
        "Software\\Microsoft\\Speech_OneCore\\Settings\\OnlineSpeechPrivacy",
        "HasAccepted",
    ) == Some(1)
}

pub fn set_online_allowed(on: bool) {
    crate::winset::set_reg_dword(
        windows::Win32::System::Registry::HKEY_CURRENT_USER,
        "Software\\Microsoft\\Speech_OneCore\\Settings\\OnlineSpeechPrivacy",
        "HasAccepted",
        on as u32,
    );
}
