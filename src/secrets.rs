//! Passwords live in the desktop keyring (Secret Service), never in the TOML file.

use keyring::Entry;

const SERVICE: &str = "hostdeck";

pub fn available() -> bool {
    Entry::store_status().is_ok()
}

pub fn status_message() -> Option<String> {
    Entry::store_status().as_ref().err().map(|e| e.to_string())
}

pub fn get(key: &str) -> Option<String> {
    Entry::new(SERVICE, key).ok()?.get_password().ok()
}

pub fn set(key: &str, password: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE, key).map_err(|e| e.to_string())?;
    entry.set_password(password).map_err(|e| e.to_string())
}

pub fn delete(key: &str) {
    if let Ok(entry) = Entry::new(SERVICE, key) {
        let _ = entry.delete_credential();
    }
}
