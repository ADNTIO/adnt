// ADNT - Dynamic CLI tool manager for ADNT projects
// Copyright (C) 2025 ADNT Sàrl <info@adnt.io>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! HTTP client construction.
//!
//! `reqwest` is built with the `rustls-no-provider` feature so that the TLS
//! stack uses the pure-Rust `ring` provider instead of `aws-lc-rs`, which
//! needs cmake and a C compiler and breaks `cargo install` on a bare Windows
//! machine. With that feature, the crypto provider must be installed before
//! any client is built, which every construction site does through this
//! module.

use std::sync::Once;

static INSTALL_PROVIDER: Once = Once::new();

/// Register `ring` as the process-wide rustls crypto provider (idempotent).
fn install_crypto_provider() {
    INSTALL_PROVIDER.call_once(|| {
        // Ignore the error: another provider was already installed, which is
        // fine as long as one exists.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A `reqwest` client builder with the crypto provider installed.
pub fn client_builder() -> reqwest::ClientBuilder {
    install_crypto_provider();
    reqwest::Client::builder()
}

/// A `reqwest` client with default settings and the crypto provider installed.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("Failed to create HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_builds_with_ring_provider() {
        let _ = client();
        let _ = client_builder().build().unwrap();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
