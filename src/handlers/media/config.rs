// Media configs: mediaConfig.js, googleTtsLanguages.js, ttsModels.js

use serde::Serialize;

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // 10MB
pub const FETCH_TIMEOUT_MS: u64 = 10_000;

pub struct ImageSignature {
    pub sig: &'static [u8],
    pub offset: usize,
    pub mime: &'static str,
    pub verify_webp: bool,
}

pub const IMAGE_SIGNATURES: &[ImageSignature] = &[
    ImageSignature { sig: &[0x89, 0x50, 0x4e, 0x47], offset: 0, mime: "image/png", verify_webp: false },
    ImageSignature { sig: &[0xff, 0xd8, 0xff], offset: 0, mime: "image/jpeg", verify_webp: false },
    ImageSignature { sig: &[0x47, 0x49, 0x46, 0x38], offset: 0, mime: "image/gif", verify_webp: false },
    ImageSignature { sig: &[0x52, 0x49, 0x46, 0x46], offset: 0, mime: "image/webp", verify_webp: true },
    ImageSignature { sig: &[0x42, 0x4d], offset: 0, mime: "image/bmp", verify_webp: false },
];

pub fn is_blocked_host(host: &str) -> bool {
    let lower = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "169.254.169.254" | "metadata.google.internal"
    )
}

pub fn detect_image_mime(bytes: &[u8]) -> Option<&'static str> {
    for entry in IMAGE_SIGNATURES {
        if bytes.len() >= entry.offset + entry.sig.len()
            && &bytes[entry.offset..entry.offset + entry.sig.len()] == entry.sig
        {
            if entry.verify_webp && !(bytes.len() >= 12 && &bytes[8..12] == b"WEBP") {
                continue;
            }
            return Some(entry.mime);
        }
    }
    None
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TtsVoiceEntry {
    pub id: &'static str,
    pub name: &'static str,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

const fn v(id: &'static str, name: &'static str) -> TtsVoiceEntry {
    TtsVoiceEntry { id, name, kind: "tts" }
}

pub const GOOGLE_TTS_LANGUAGES: &[TtsVoiceEntry] = &[
    v("af", "Afrikaans"), v("ar", "Arabic"), v("bg", "Bulgarian"), v("bn", "Bengali"),
    v("bs", "Bosnian"), v("ca", "Catalan"), v("cs", "Czech"), v("cy", "Welsh"),
    v("da", "Danish"), v("de", "German"), v("el", "Greek"), v("en", "English"),
    v("eo", "Esperanto"), v("es", "Spanish"), v("et", "Estonian"), v("fi", "Finnish"),
    v("fr", "French"), v("gu", "Gujarati"), v("hi", "Hindi"), v("hr", "Croatian"),
    v("hu", "Hungarian"), v("hy", "Armenian"), v("id", "Indonesian"), v("is", "Icelandic"),
    v("it", "Italian"), v("ja", "Japanese"), v("jw", "Javanese"), v("km", "Khmer"),
    v("kn", "Kannada"), v("ko", "Korean"), v("la", "Latin"), v("lv", "Latvian"),
    v("mk", "Macedonian"), v("ml", "Malayalam"), v("mr", "Marathi"), v("my", "Myanmar (Burmese)"),
    v("ne", "Nepali"), v("nl", "Dutch"), v("no", "Norwegian"), v("pl", "Polish"),
    v("pt", "Portuguese"), v("ro", "Romanian"), v("ru", "Russian"), v("si", "Sinhala"),
    v("sk", "Slovak"), v("sq", "Albanian"), v("sr", "Serbian"), v("su", "Sundanese"),
    v("sv", "Swedish"), v("sw", "Swahili"), v("ta", "Tamil"), v("te", "Telugu"),
    v("th", "Thai"), v("tl", "Filipino"), v("tr", "Turkish"), v("uk", "Ukrainian"),
    v("ur", "Urdu"), v("vi", "Vietnamese"), v("zh-CN", "Chinese (Simplified)"),
    v("zh-TW", "Chinese (Traditional)"),
];

pub const OPENAI_VOICES_STANDARD: &[TtsVoiceEntry] = &[
    v("alloy", "Alloy"), v("ash", "Ash"), v("coral", "Coral"),
    v("echo", "Echo"), v("fable", "Fable"), v("nova", "Nova"),
    v("onyx", "Onyx"), v("sage", "Sage"), v("shimmer", "Shimmer"),
];

pub const OPENAI_VOICES_FULL: &[TtsVoiceEntry] = &[
    v("alloy", "Alloy"), v("ash", "Ash"), v("ballad", "Ballad"), v("cedar", "Cedar"),
    v("coral", "Coral"), v("echo", "Echo"), v("fable", "Fable"), v("marin", "Marin"),
    v("nova", "Nova"), v("onyx", "Onyx"), v("sage", "Sage"), v("shimmer", "Shimmer"),
    v("verse", "Verse"),
];

pub const GEMINI_VOICES: &[TtsVoiceEntry] = &[
    v("Zephyr", "Zephyr"), v("Puck", "Puck"), v("Charon", "Charon"), v("Kore", "Kore"),
    v("Fenrir", "Fenrir"), v("Leda", "Leda"), v("Orus", "Orus"), v("Aoede", "Aoede"),
    v("Callirrhoe", "Callirrhoe"), v("Autonoe", "Autonoe"), v("Enceladus", "Enceladus"),
    v("Iapetus", "Iapetus"), v("Umbriel", "Umbriel"), v("Algieba", "Algieba"),
    v("Despina", "Despina"), v("Erinome", "Erinome"), v("Algenib", "Algenib"),
    v("Rasalgethi", "Rasalgethi"), v("Laomedeia", "Laomedeia"), v("Achernar", "Achernar"),
    v("Alnilam", "Alnilam"), v("Schedar", "Schedar"), v("Gacrux", "Gacrux"),
    v("Pulcherrima", "Pulcherrima"), v("Achird", "Achird"), v("Zubenelgenubi", "Zubenelgenubi"),
    v("Vindemiatrix", "Vindemiatrix"), v("Sadachbia", "Sadachbia"),
    v("Sadaltager", "Sadaltager"), v("Sulafat", "Sulafat"),
];

pub const MIMO_VOICES: &[TtsVoiceEntry] = &[
    v("mimo_default", "mimo_default"),
    v("冰糖", "冰糖"), v("茉莉", "茉莉"), v("苏打", "苏打"), v("白桦", "白桦"),
    v("Mia", "Mia"), v("Chloe", "Chloe"), v("Milo", "Milo"), v("Dean", "Dean"),
];

pub const EDGE_TTS_DEFAULTS: &[TtsVoiceEntry] = &[
    v("en-US-AriaNeural", "Aria (en-US)"),
    v("en-US-GuyNeural", "Guy (en-US)"),
    v("en-GB-SoniaNeural", "Sonia (en-GB)"),
    v("vi-VN-HoaiMyNeural", "Hoai My (vi-VN)"),
    v("vi-VN-NamMinhNeural", "Nam Minh (vi-VN)"),
    v("zh-CN-XiaoxiaoNeural", "Xiaoxiao (zh-CN)"),
    v("zh-CN-YunxiNeural", "Yunxi (zh-CN)"),
    v("fr-FR-DeniseNeural", "Denise (fr-FR)"),
    v("de-DE-KatjaNeural", "Katja (de-DE)"),
    v("ja-JP-NanamiNeural", "Nanami (ja-JP)"),
    v("ko-KR-SunHiNeural", "SunHi (ko-KR)"),
];

pub const LOCAL_DEVICE_DEFAULTS: &[TtsVoiceEntry] = &[v("default", "System Default Voice")];

pub const PHONON_VOICES_FULL: &[TtsVoiceEntry] = &[
    v("default", "Default (Neutral)"),
    v("Zoey", "Zoey (US Female)"), v("Juniper", "Juniper (US Female)"),
    v("Everly", "Everly (US Female)"), v("Harper", "Harper (US Female)"),
    v("Hadley", "Hadley (US Female)"), v("Delia", "Delia (US Female)"),
    v("Easton", "Easton (US Male)"), v("Micah", "Micah (US Male)"),
    v("Trevor", "Trevor (US Male)"), v("Russell", "Russell (US Male)"),
    v("Preston", "Preston (US Male)"), v("Miles", "Miles (US Male)"),
    v("Maeve", "Maeve (UK Female)"), v("Freya", "Freya (UK Female)"),
    v("Pippa", "Pippa (UK Female)"), v("Elodie-Rose", "Elodie-Rose (UK Female)"),
    v("Imogen", "Imogen (UK Female)"), v("Lottie", "Lottie (UK Female)"),
    v("Bryony", "Bryony (UK Female)"),
    v("Alfie", "Alfie (UK Male)"), v("Hugh", "Hugh (UK Male)"),
    v("Edwin", "Edwin (UK Male)"), v("Spencer", "Spencer (UK Male)"),
    v("Toby", "Toby (UK Male)"), v("Teddy", "Teddy (UK Male)"),
    v("Azgorath", "Azgorath (Character)"),
    v("Captain Silas Blackwater", "Captain Silas (Character)"),
    v("Fairy Godmother", "Fairy Godmother (Character)"),
    v("Fennick the Young Elf", "Fennick Elf (Character)"),
    v("Grub the Ogre", "Grub the Ogre (Character)"),
    v("Master Aldric", "Master Aldric (Character)"),
    v("Morwenna of the Deep", "Morwenna (Character)"),
    v("Old Mortlake", "Old Mortlake (Character)"),
    v("Sir Bramwell", "Sir Bramwell (Character)"),
    v("The Snow Queen", "Snow Queen (Character)"),
    v("The Thing Under the Stairs", "Thing Under Stairs (Character)"),
    v("Vyrmathrax the Ancient", "Vyrmathrax (Character)"),
];

pub const PHONON_DEFAULT_VOICE: &[TtsVoiceEntry] = &[v("default", "Default")];

pub fn get_tts_voices_for_model(provider: &str, model_id: &str) -> Option<&'static [TtsVoiceEntry]> {
    match provider {
        "openai" | "openrouter" => match model_id {
            "tts-1" | "tts-1-hd" | "openai/tts-1" | "openai/tts-1-hd" => Some(OPENAI_VOICES_STANDARD),
            _ => Some(OPENAI_VOICES_FULL),
        },
        "gemini" => Some(GEMINI_VOICES),
        "xiaomi-mimo" => Some(MIMO_VOICES),
        "edge-tts" => Some(EDGE_TTS_DEFAULTS),
        "local-device" => Some(LOCAL_DEVICE_DEFAULTS),
        "google-tts" => Some(GOOGLE_TTS_LANGUAGES),
        "ollama-local" => match model_id {
            "phonon:latest" | "phonon" | "phonon:en" | "phonon:31m-en" | "phonon:40m-en" => {
                Some(PHONON_VOICES_FULL)
            }
            _ => Some(PHONON_DEFAULT_VOICE),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_image_magic_bytes_and_blocked_hosts() {
        assert_eq!(detect_image_mime(&[0x89, 0x50, 0x4e, 0x47, 0x0d]), Some("image/png"));
        assert_eq!(detect_image_mime(b"RIFF\x00\x00\x00\x00WEBP"), Some("image/webp"));
        assert_eq!(detect_image_mime(b"RIFF\x00\x00\x00\x00AVI "), None);
        assert!(is_blocked_host("169.254.169.254"));
        assert!(!is_blocked_host("example.com"));
    }

    #[test]
    fn tts_voices_lookup() {
        assert_eq!(get_tts_voices_for_model("openai", "tts-1").unwrap().len(), 9);
        assert_eq!(get_tts_voices_for_model("openai", "gpt-4o-mini-tts").unwrap().len(), 13);
        assert_eq!(get_tts_voices_for_model("gemini", "gemini-2.5-flash-preview-tts").unwrap().len(), 30);
    }
}
