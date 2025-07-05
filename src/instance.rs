use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bollard::secret::ContainerSummary;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SquittalInstance {
    pub name: String,
    pub created_by: String,
    pub created_on: SystemTime,
    pub port: u16,
}

impl From<ContainerSummary> for SquittalInstance {
    fn from(summary: ContainerSummary) -> Self {
        let epoch: i64 = summary.created.expect("failed to get created of container");

        let owner = summary
            .labels
            .expect("missing labels field")
            .get("created_by")
            .expect("missing created_by label")
            .clone();

        let port = summary
            .ports
            .expect("missing ports")
            .iter()
            .find(|&iter| iter.private_port == 8080)
            .expect("missing port 8080")
            .public_port
            .expect("missing public_port for 8080");

        return SquittalInstance {
            name: summary.names.unwrap()[0].clone(),
            created_by: owner,
            created_on: UNIX_EPOCH + Duration::from_secs(epoch as u64),
            port: port,
        };
    }
}
