//! Integration registry for routing URLs to integrations.
//!
//! The [`IntegrationRegistry`] maintains a mapping from domain names to integrations,
//! allowing efficient lookup of which integration should handle a given URL.

use std::collections::HashMap;

use url::Url;

use crate::{Integration, IntegrationName};

/// Error that occurs when registering an integration.
#[derive(Debug, Clone, thiserror::Error)]
#[error("domain {domain} already registered to {existing:?}, cannot register to {new:?}")]
pub struct RegistrationError {
    pub domain: &'static str,
    pub existing: IntegrationName,
    pub new: IntegrationName,
}

/// Registry that routes URLs to appropriate integrations.
#[derive(Clone)]
pub struct IntegrationRegistry {
    /// Domain-specific integrations.
    by_domain: HashMap<&'static str, Integration>,
}

impl IntegrationRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_domain: HashMap::new(),
        }
    }

    /// Register an integration for all its claimed domains.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError`] if any domain is already registered.
    /// This is a programming error - domains should not overlap between integrations.
    pub fn register(&mut self, integration: Integration) -> Result<(), RegistrationError> {
        // Check for overlaps
        for domain in integration.domains() {
            if let Some(existing) = self.by_domain.get(domain) {
                return Err(RegistrationError {
                    domain,
                    existing: existing.name(),
                    new: integration.name(),
                });
            }
        }

        // No conflicts, insert all domains
        for domain in integration.domains() {
            self.by_domain.insert(domain, integration.clone());
        }

        Ok(())
    }

    /// Get the integration for a URL's domain, if any specialized one exists.
    ///
    /// Returns `None` if no integration is registered for the domain.
    /// The caller should use a generic fetcher for such URLs.
    #[must_use]
    pub fn get(&self, url: &Url) -> Option<&Integration> {
        url.host_str().and_then(|host| self.get_by_domain(host))
    }

    /// Get integration name for a domain (for computing `worker_affinity`).
    ///
    /// Returns `None` if no integration claims this domain.
    #[must_use]
    pub fn integration_name_for_domain(&self, domain: &str) -> Option<IntegrationName> {
        self.get_by_domain(domain).map(Integration::name)
    }

    /// Look up integration by domain, trying exact match then without www. prefix.
    fn get_by_domain(&self, domain: &str) -> Option<&Integration> {
        self.by_domain
            .get(domain)
            .or_else(|| self.by_domain.get(domain.strip_prefix("www.")?))
    }

    /// Normalize a URL using the appropriate integration's rules.
    ///
    /// If no integration claims the URL's domain, returns the URL unchanged.
    /// This allows generic tracking parameter removal to be applied by the caller
    /// regardless of whether a specialized integration exists.
    #[must_use]
    pub fn normalize_url(&self, url: &Url) -> Url {
        self.get(url)
            .map_or_else(|| url.clone(), |i| i.normalize_url(url))
    }
}

impl Default for IntegrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::content::FetchedContent;
    use crate::http::HttpClient;
    use crate::{FetchError, IntegrationMeta, SingleFetcher};

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Test integration for unit tests.
    struct TestIntegration {
        name: IntegrationName,
        domains: &'static [&'static str],
    }

    impl IntegrationMeta for TestIntegration {
        fn name(&self) -> IntegrationName {
            self.name
        }

        fn domains(&self) -> &'static [&'static str] {
            self.domains
        }
    }

    #[async_trait::async_trait]
    impl SingleFetcher for TestIntegration {
        async fn fetch(
            &self,
            _http: &dyn HttpClient,
            _url: &Url,
        ) -> Result<FetchedContent, FetchError> {
            Ok(FetchedContent::default())
        }
    }

    fn test_integration(name: IntegrationName, domains: &'static [&'static str]) -> Integration {
        Integration::Single(Arc::new(TestIntegration { name, domains }))
    }

    #[test]
    fn test_registry_exact_domain_match() -> TestResult {
        let mut registry = IntegrationRegistry::new();
        registry.register(test_integration(
            IntegrationName::Reddit,
            &["reddit.com", "redd.it"],
        ))?;

        let url = Url::parse("https://reddit.com/r/rust")?;
        assert!(registry.get(&url).is_some());

        let url = Url::parse("https://redd.it/abc123")?;
        assert!(registry.get(&url).is_some());
        Ok(())
    }

    #[test]
    fn test_registry_strips_www_prefix() -> TestResult {
        let mut registry = IntegrationRegistry::new();
        registry.register(test_integration(IntegrationName::Reddit, &["example.com"]))?;

        let url = Url::parse("https://www.example.com/page")?;
        assert!(registry.get(&url).is_some());

        let url = Url::parse("https://example.com/page")?;
        assert!(registry.get(&url).is_some());
        Ok(())
    }

    #[test]
    fn test_registry_returns_none_for_unknown_domain() -> TestResult {
        let mut registry = IntegrationRegistry::new();
        registry.register(test_integration(IntegrationName::Reddit, &["reddit.com"]))?;

        let url = Url::parse("https://unknown-site.com/page")?;
        assert!(registry.get(&url).is_none());
        Ok(())
    }

    #[test]
    fn test_registry_handles_url_without_host() -> TestResult {
        let registry = IntegrationRegistry::new();

        // file:// URL has no host
        let url = Url::parse("file:///path/to/file")?;
        assert!(registry.get(&url).is_none());
        Ok(())
    }

    #[test]
    fn test_integration_name_for_domain() -> TestResult {
        let mut registry = IntegrationRegistry::new();
        registry.register(test_integration(
            IntegrationName::Reddit,
            &["reddit.com", "redd.it"],
        ))?;

        assert_eq!(
            registry.integration_name_for_domain("reddit.com"),
            Some(IntegrationName::Reddit)
        );
        assert_eq!(
            registry.integration_name_for_domain("www.reddit.com"),
            Some(IntegrationName::Reddit)
        );
        assert_eq!(
            registry.integration_name_for_domain("redd.it"),
            Some(IntegrationName::Reddit)
        );
        assert_eq!(registry.integration_name_for_domain("unknown.com"), None);
        Ok(())
    }

    #[test]
    fn test_registry_errors_on_domain_overlap() -> TestResult {
        let mut registry = IntegrationRegistry::new();
        registry.register(test_integration(IntegrationName::Reddit, &["reddit.com"]))?;

        // Any integration trying to claim an already-registered domain should fail
        let result = registry.register(test_integration(
            IntegrationName::Instagram,
            &["reddit.com"],
        ));
        let err = match result {
            Ok(()) => return Err("expected registration to fail".into()),
            Err(e) => e,
        };
        assert_eq!(err.domain, "reddit.com");
        assert_eq!(err.existing, IntegrationName::Reddit);
        assert_eq!(err.new, IntegrationName::Instagram);
        Ok(())
    }
}
