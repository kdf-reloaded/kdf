//! GitHub API client implementing `RepositoryOperations`.

use async_trait::async_trait;
use mm2_err_handle::prelude::MmError;
use mm2_net::transport::slurp_url_with_headers;
use serde::de::DeserializeOwned;

use crate::{FileMetadata, GitCommons, GitControllerError, RepositoryOperations};

const GITHUB_CLIENT_USER_AGENT: &str = "mm2";

/// GitHub-specific implementation of `RepositoryOperations`.
pub struct GithubClient {
    api_address: String,
}

impl GitCommons for GithubClient {
    fn new(api_address: String) -> Self { Self { api_address } }
}

#[async_trait]
impl RepositoryOperations for GithubClient {
    async fn deserialize_json_source<T>(&self, file_metadata: FileMetadata) -> Result<T, MmError<GitControllerError>>
    where
        T: DeserializeOwned,
    {
        let (_status_code, _headers, data) = slurp_url_with_headers(&file_metadata.download_url, vec![(
            http::header::USER_AGENT.as_str(),
            GITHUB_CLIENT_USER_AGENT,
        )])
        .await
        .map_err(|e| GitControllerError::HttpError(e.to_string()))?;

        serde_json::from_slice(&data).map_err(|e| MmError::new(GitControllerError::DeserializationError(e.to_string())))
    }

    async fn get_file_metadata_list(
        &self,
        owner: &str,
        repository_name: &str,
        branch: &str,
        dir: &str,
    ) -> Result<Vec<FileMetadata>, MmError<GitControllerError>> {
        let uri = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.api_address, owner, repository_name, dir, branch
        );

        let (_status_code, _headers, data) = slurp_url_with_headers(&uri, vec![(
            http::header::USER_AGENT.as_str(),
            GITHUB_CLIENT_USER_AGENT,
        )])
        .await
        .map_err(|e| GitControllerError::HttpError(e.to_string()))?;

        serde_json::from_slice(&data).map_err(|e| MmError::new(GitControllerError::DeserializationError(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GitController, GITHUB_API_URI};

    #[test]
    fn test_git_controller_creation() {
        let controller: GitController<GithubClient> = GitController::new(GITHUB_API_URI);
        assert_eq!(controller.client.api_address, GITHUB_API_URI);
    }

    #[test]
    fn test_file_metadata_deserialization() {
        let json = r#"[
            {"name": "test.json", "download_url": "https://example.com/test.json", "size": 42}
        ]"#;
        let metadata: Vec<FileMetadata> = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].name, "test.json");
        assert_eq!(metadata[0].size, 42);
    }
}
