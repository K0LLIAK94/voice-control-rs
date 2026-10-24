use anyhow::{Context, Result, bail};
use libloading::Library;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
};

type ModelNew = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type ModelFree = unsafe extern "C" fn(*mut c_void);
type RecognizerNew = unsafe extern "C" fn(*mut c_void, f32, *const c_char) -> *mut c_void;
type Accept = unsafe extern "C" fn(*mut c_void, *const i16, i32) -> i32;
type Text = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type SetWords = unsafe extern "C" fn(*mut c_void, i32);
type FindWord = unsafe extern "C" fn(*mut c_void, *const c_char) -> i32;

#[cfg(windows)]
fn load_library(library_path: &Path) -> Result<Library> {
    // Config paths use '/', while Path::join on Windows preserves those
    // separators. LoadLibraryExW accepts that filename but its
    // LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR dependency search then fails with 126.
    // Canonicalize to a native absolute path before loading sibling DLLs.
    let canonical = std::fs::canonicalize(library_path).with_context(|| {
        format!(
            "Не найдена библиотека распознавания {}",
            library_path.display()
        )
    })?;
    // SAFETY: load only the selected absolute DLL and use its own directory plus
    // Windows safe default directories. No process-wide DLL search changes.
    unsafe {
        libloading::os::windows::Library::load_with_flags(&canonical, 0x100 | 0x1000)
            .map(Into::into)
            .with_context(|| {
                format!(
                    "Не удалось загрузить {} и её зависимости",
                    canonical.display()
                )
            })
    }
}

pub struct Recognizer {
    _library: Library,
    model: *mut c_void,
    recognizer: *mut c_void,
    model_free: ModelFree,
    recognizer_free: ModelFree,
    accept: Accept,
    result: Text,
    final_result: Text,
    reset: ModelFree,
}

impl Recognizer {
    pub fn new(library_path: &Path, model_path: &Path, grammar: &[String]) -> Result<Self> {
        if !model_path.join("am/final.mdl").is_file() {
            bail!(
                "Не найдена модель {}. Запустите scripts/bootstrap.ps1",
                model_path.display()
            );
        }
        // Resolve only the chosen DLL's dependencies and Windows default safe directories.
        #[cfg(windows)]
        let library: Library = load_library(library_path)?;
        #[cfg(not(windows))]
        let library = unsafe { Library::new(library_path)? };
        unsafe {
            let model_new: ModelNew = *library.get(b"vosk_model_new\0")?;
            let model_free: ModelFree = *library.get(b"vosk_model_free\0")?;
            let recognizer_new: RecognizerNew = *library.get(b"vosk_recognizer_new_grm\0")?;
            let recognizer_free: ModelFree = *library.get(b"vosk_recognizer_free\0")?;
            let accept: Accept = *library.get(b"vosk_recognizer_accept_waveform_s\0")?;
            let result: Text = *library.get(b"vosk_recognizer_result\0")?;
            let final_result: Text = *library.get(b"vosk_recognizer_final_result\0")?;
            let reset: ModelFree = *library.get(b"vosk_recognizer_reset\0")?;
            let words: SetWords = *library.get(b"vosk_recognizer_set_words\0")?;
            let find_word: FindWord = *library.get(b"vosk_model_find_word\0")?;
            let log_level: unsafe extern "C" fn(i32) = *library.get(b"vosk_set_log_level\0")?;
            log_level(-1);
            let path = CString::new(model_path.to_string_lossy().as_bytes())?;
            let model = model_new(path.as_ptr());
            if model.is_null() {
                bail!("Vosk не смог загрузить модель {}", model_path.display());
            }
            // Vosk silently drops unknown grammar words. Reject that configuration instead.
            for phrase in grammar.iter().filter(|s| s.as_str() != "[unk]") {
                for word in phrase.split_whitespace() {
                    if find_word(model, CString::new(word)?.as_ptr()) < 0 {
                        model_free(model);
                        bail!(
                            "Слово «{word}» отсутствует в словаре модели; используйте другую формулировку в config.toml"
                        );
                    }
                }
            }
            let grammar = CString::new(serde_json::to_string(grammar)?)?;
            let recognizer = recognizer_new(model, 16_000.0, grammar.as_ptr());
            if recognizer.is_null() {
                model_free(model);
                bail!("Не удалось создать распознаватель Vosk");
            }
            words(recognizer, 1);
            Ok(Self {
                _library: library,
                model,
                recognizer,
                model_free,
                recognizer_free,
                accept,
                result,
                final_result,
                reset,
            })
        }
    }

    pub fn feed(&mut self, samples: &[i16]) -> Result<Option<crate::commands::Recognition>> {
        let status =
            unsafe { (self.accept)(self.recognizer, samples.as_ptr(), samples.len().try_into()?) };
        match status {
            -1 => bail!("Vosk: ошибка обработки звука"),
            0 => Ok(None),
            _ => self.read(self.result).map(Some),
        }
    }
    pub fn finish(&mut self) -> Result<crate::commands::Recognition> {
        self.read(self.final_result)
    }
    pub fn utterance(&mut self, samples: &[i16]) -> Result<crate::commands::Recognition> {
        unsafe {
            (self.reset)(self.recognizer);
        }
        // A bounded utterance is decoded as a whole to keep negations attached to commands.
        if let Some(result) = self.feed(samples)? {
            return Ok(result);
        }
        self.finish()
    }
    fn read(&self, function: Text) -> Result<crate::commands::Recognition> {
        let pointer = unsafe { function(self.recognizer) };
        if pointer.is_null() {
            bail!("Vosk вернул пустой результат");
        }
        let json = unsafe { CStr::from_ptr(pointer) }.to_str()?;
        serde_json::from_str(json).context("Ошибка ответа Vosk")
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        unsafe {
            (self.recognizer_free)(self.recognizer);
            (self.model_free)(self.model);
        }
    }
}

#[cfg(all(test, windows))]
mod loader_tests {
    use super::*;

    // Run after scripts/bootstrap.ps1:
    // cargo test bundled_vosk_forward_slashes -- --ignored
    #[test]
    #[ignore = "requires the downloaded Windows Vosk runtime and sibling DLLs"]
    fn bundled_vosk_forward_slashes() {
        let mixed = format!("{}/runtime/libvosk.dll", env!("CARGO_MANIFEST_DIR"));
        let library =
            load_library(Path::new(&mixed)).expect("load Vosk with config-style separators");
        // Validate a real export, so opening an unrelated placeholder can't pass.
        unsafe {
            library
                .get::<unsafe extern "C" fn(i32)>(b"vosk_set_log_level\0")
                .expect("loaded DLL must export the Vosk C API");
        }
    }
}
