//! Azure Key Vault secret store, authenticated with the ambient identity:
//! the user-assigned managed identity on Container Apps (its client id
//! arrives in `AZURE_CLIENT_ID`), or the developer's `az login` locally.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_core::http::StatusCode;
use azure_identity::{
    DeveloperToolsCredential, ManagedIdentityCredential, ManagedIdentityCredentialOptions,
    UserAssignedId,
};
use azure_security_keyvault_secrets::SecretClient;
use azure_security_keyvault_secrets::models::SetSecretParameters;
use url::Url;

use super::SecretStore;

pub struct KeyVaultSecrets {
    client: SecretClient,
}

impl KeyVaultSecrets {
    pub fn new(vault_url: &Url) -> Result<Self> {
        let credential = credential()?;
        let client = SecretClient::new(vault_url.as_str(), credential, None)
            .context("creating Key Vault client")?;
        Ok(Self { client })
    }
}

/// Managed identity when one is configured for the process, developer
/// tooling (`az login`) otherwise.
fn credential() -> Result<Arc<dyn TokenCredential>> {
    match std::env::var("AZURE_CLIENT_ID") {
        Ok(client_id) if !client_id.is_empty() => {
            tracing::info!("Key Vault: using user-assigned managed identity");
            let options = ManagedIdentityCredentialOptions {
                user_assigned_id: Some(UserAssignedId::ClientId(client_id)),
                ..Default::default()
            };
            Ok(ManagedIdentityCredential::new(Some(options))
                .context("managed identity credential")?)
        }
        _ => {
            tracing::info!("Key Vault: using developer tools credential (az login)");
            Ok(DeveloperToolsCredential::new(None).context("developer tools credential")?)
        }
    }
}

#[async_trait]
impl SecretStore for KeyVaultSecrets {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        match self.client.get_secret(name, None).await {
            Ok(response) => {
                let secret = response.into_model().context("decoding secret")?;
                Ok(secret.value)
            }
            Err(e) if e.http_status() == Some(StatusCode::NotFound) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading secret {name}")),
        }
    }

    async fn set(&self, name: &str, value: &str) -> Result<()> {
        let params = SetSecretParameters {
            value: Some(value.to_string()),
            ..Default::default()
        };
        self.client
            .set_secret(name, params.try_into()?, None)
            .await
            .with_context(|| format!("writing secret {name}"))?;
        Ok(())
    }
}
