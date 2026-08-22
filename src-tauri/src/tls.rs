//! Process-wide TLS provider selection.
//!
//! reqwest 0.13 deliberately allows the application to choose a rustls
//! provider. Install `ring` before Tauri creates any network client so market,
//! preset-download, updater, and future TLS users cannot depend on feature
//! unification or silently select a native-TLS backend.

pub fn install_process_default() -> Result<(), String> {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return Ok(());
    }

    match rustls::crypto::ring::default_provider().install_default() {
        Ok(()) => Ok(()),
        // Another initializer can only win this race before Tauri starts in a
        // test or an embedding context. A provider is now present either way.
        Err(_) if rustls::crypto::CryptoProvider::get_default().is_some() => Ok(()),
        Err(_) => Err("could not install the rustls ring provider".to_string()),
    }
}

/// The only construction path for Desktop-owned reqwest clients.
///
/// `main` installs the provider before Tauri starts, but unit tests and small
/// command helpers can construct a client without entering `main`. Keeping
/// this guard at the builder boundary makes that safe and prevents a future
/// call site from accidentally relying on feature unification.
pub(crate) fn client_builder() -> Result<reqwest::ClientBuilder, String> {
    install_process_default()?;
    Ok(reqwest::Client::builder())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_initialization_is_idempotent() {
        assert!(install_process_default().is_ok());
        assert!(install_process_default().is_ok());
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn client_builder_initializes_before_constructing_reqwest() {
        let client =
            client_builder().and_then(|builder| builder.build().map_err(|error| error.to_string()));
        assert!(client.is_ok());
    }
}
