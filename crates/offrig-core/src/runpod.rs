//! RunPod API client: REST (`rest.runpod.io/v1`) for pods and network volumes,
//! GraphQL (`api.runpod.io/graphql`) for GPU prices, stock and the account balance,
//! which REST does not expose.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};

pub const REST_BASE: &str = "https://rest.runpod.io/v1";
pub const GRAPHQL_URL: &str = "https://api.runpod.io/graphql";

pub struct RunPod {
    agent: ureq::Agent,
    key: String,
    rest_base: String,
    graphql_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pod {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub desired_status: String,
    #[serde(default, alias = "imageName")]
    pub image: String,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub port_mappings: Option<BTreeMap<String, u16>>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default, deserialize_with = "lenient_f64")]
    pub cost_per_hr: f64,
    #[serde(default)]
    pub gpu_count: u32,
    #[serde(default)]
    pub last_started_at: Option<String>,
    #[serde(default)]
    pub network_volume_id: Option<String>,
    #[serde(default)]
    pub machine: Option<Machine>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    #[serde(default)]
    pub gpu_type_id: Option<String>,
    #[serde(default)]
    pub data_center_id: Option<String>,
    /// The host driver's CUDA version, if the pod API reports one on the machine.
    /// Read tolerantly: the field's name and presence are not confirmed against a live
    /// response, so `None` means "not reported", never "unknown host is fine".
    #[serde(default, deserialize_with = "lenient_string")]
    pub cuda_version: Option<String>,
}

/// A string field RunPod may send as a string or a bare number (`"12.8"` or `12.8`).
fn lenient_string<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

impl Pod {
    pub fn is_running(&self) -> bool {
        self.desired_status == "RUNNING"
    }

    /// `(public ip, host port mapped to 22/tcp)`, once RunPod has assigned both.
    pub fn ssh_endpoint(&self) -> Option<(String, u16)> {
        let ip = self.public_ip.as_deref().filter(|ip| !ip.is_empty())?;
        let port = *self.port_mappings.as_ref()?.get("22")?;
        Some((ip.to_string(), port))
    }

    pub fn gpu_type(&self) -> Option<&str> {
        self.machine.as_ref()?.gpu_type_id.as_deref()
    }

    /// The host driver's CUDA version when the pod API reports it.
    pub fn host_cuda(&self) -> Option<&str> {
        self.machine.as_ref()?.cuda_version.as_deref()
    }
}

/// The body of `POST /pods`. Field names follow RunPod's `PodCreateInput`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PodCreate {
    pub name: String,
    pub image_name: String,
    pub gpu_type_ids: Vec<String>,
    pub gpu_type_priority: String,
    pub gpu_count: u32,
    pub cloud_type: String,
    pub support_public_ip: bool,
    pub ports: Vec<String>,
    pub container_disk_in_gb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_in_gb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_volume_id: Option<String>,
    pub volume_mount_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub data_center_ids: Vec<String>,
    /// Hosts whose CUDA (driver) version is in this list; empty means any host.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_cuda_versions: Vec<String>,
    pub docker_entrypoint: Vec<String>,
    pub docker_start_cmd: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkVolume {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u32,
    #[serde(default)]
    pub data_center_id: String,
}

/// One GPU type's offer for a given GPU count. `price_per_hr` is the total for all
/// GPUs, and `None` means no machine has that many free right now.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuOffer {
    pub id: String,
    pub display_name: String,
    pub memory_gb: u32,
    pub gpu_count: u32,
    pub price_per_hr: Option<f64>,
    pub stock: Option<String>,
}

impl GpuOffer {
    pub fn total_vram_gb(&self) -> u32 {
        self.memory_gb * self.gpu_count
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    #[serde(default)]
    pub client_balance: f64,
    #[serde(default)]
    pub current_spend_per_hr: f64,
    #[serde(default)]
    pub spend_limit: Option<f64>,
}

impl Account {
    /// Hours until the balance reaches zero at `extra_per_hr` on top of current spend.
    pub fn runway_hours(&self, extra_per_hr: f64) -> Option<f64> {
        let rate = self.current_spend_per_hr + extra_per_hr;
        (rate > 0.0).then(|| self.client_balance / rate)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DataCenter {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub storage_support: bool,
}

impl RunPod {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("RUNPOD_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or(Error::MissingApiKey)?;
        // Tests point the binaries at a mock RunPod. Debug builds only: a release
        // binary can never be redirected to send the key somewhere else.
        #[cfg(debug_assertions)]
        if let Ok(base) = std::env::var("OFFRIG_TEST_RUNPOD_BASE")
            && !base.is_empty()
        {
            return Ok(Self::new(key, &base, &format!("{base}/graphql")));
        }
        Ok(Self::new(key, REST_BASE, GRAPHQL_URL))
    }

    pub fn new(key: impl Into<String>, rest_base: &str, graphql_url: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .new_agent();
        Self {
            agent,
            key: key.into(),
            rest_base: rest_base.trim_end_matches('/').to_string(),
            graphql_url: graphql_url.to_string(),
        }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.key)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.rest_base, path)
    }

    pub fn list_pods(&self) -> Result<Vec<Pod>> {
        let resp = self
            .agent
            .get(self.url("/pods?includeMachine=true"))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("list pods", e))?;
        read_json(resp, "list pods")
    }

    pub fn get_pod(&self, id: &str) -> Result<Pod> {
        let resp = self
            .agent
            .get(self.url(&format!("/pods/{id}?includeMachine=true")))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("get pod", e))?;
        read_json(resp, &format!("get pod {id}"))
    }

    /// Find a pod by exact id or exact name.
    pub fn find_pod(&self, id_or_name: &str) -> Result<Pod> {
        self.list_pods()?
            .into_iter()
            .find(|p| p.id == id_or_name || p.name == id_or_name)
            .ok_or_else(|| Error::PodNotFound(id_or_name.to_string()))
    }

    pub fn create_pod(&self, spec: &PodCreate) -> Result<Pod> {
        let resp = self
            .agent
            .post(self.url("/pods"))
            .header("Authorization", self.auth())
            .send_json(spec)
            .map_err(|e| Error::http("create pod", e))?;
        read_json(resp, &format!("create pod {}", spec.name))
    }

    /// Terminate a pod. Its container disk is gone afterwards; a network volume is not.
    pub fn delete_pod(&self, id: &str) -> Result<()> {
        let resp = self
            .agent
            .delete(self.url(&format!("/pods/{id}")))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("delete pod", e))?;
        read_ok(resp, &format!("delete pod {id}"))
    }

    pub fn stop_pod(&self, id: &str) -> Result<()> {
        let resp = self
            .agent
            .post(self.url(&format!("/pods/{id}/stop")))
            .header("Authorization", self.auth())
            .send_empty()
            .map_err(|e| Error::http("stop pod", e))?;
        read_ok(resp, &format!("stop pod {id}"))
    }

    pub fn list_volumes(&self) -> Result<Vec<NetworkVolume>> {
        let resp = self
            .agent
            .get(self.url("/networkvolumes"))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("list network volumes", e))?;
        read_json(resp, "list network volumes")
    }

    pub fn create_volume(
        &self,
        name: &str,
        size_gb: u32,
        data_center_id: &str,
    ) -> Result<NetworkVolume> {
        let body =
            serde_json::json!({ "name": name, "size": size_gb, "dataCenterId": data_center_id });
        let resp = self
            .agent
            .post(self.url("/networkvolumes"))
            .header("Authorization", self.auth())
            .send_json(&body)
            .map_err(|e| Error::http("create network volume", e))?;
        read_json(resp, &format!("create network volume {name}"))
    }

    pub fn delete_volume(&self, id: &str) -> Result<()> {
        let resp = self
            .agent
            .delete(self.url(&format!("/networkvolumes/{id}")))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("delete network volume", e))?;
        read_ok(resp, &format!("delete network volume {id}"))
    }

    fn graphql<T: DeserializeOwned>(&self, query: &str, what: &str) -> Result<T> {
        #[derive(Deserialize)]
        struct Resp<T> {
            data: Option<T>,
            #[serde(default)]
            errors: Vec<GqlError>,
        }
        #[derive(Deserialize)]
        struct GqlError {
            message: String,
        }
        let resp = self
            .agent
            .post(&self.graphql_url)
            .header("Authorization", self.auth())
            .send_json(serde_json::json!({ "query": query }))
            .map_err(|e| Error::http(what, e))?;
        let parsed: Resp<T> = read_json(resp, what)?;
        if !parsed.errors.is_empty() {
            let message = parsed
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::GraphQl {
                what: what.to_string(),
                message,
            });
        }
        parsed.data.ok_or_else(|| Error::GraphQl {
            what: what.to_string(),
            message: "response carried no data".to_string(),
        })
    }

    pub fn account(&self) -> Result<Account> {
        #[derive(Deserialize)]
        struct Data {
            myself: Account,
        }
        let d: Data = self.graphql(
            "query { myself { clientBalance currentSpendPerHr spendLimit } }",
            "account balance",
        )?;
        Ok(d.myself)
    }

    /// Secure-cloud offers for every GPU type at `gpu_count`, cheapest available first.
    pub fn gpu_offers(&self, gpu_count: u32) -> Result<Vec<GpuOffer>> {
        self.gpu_offers_in(gpu_count, None)
    }

    /// Offers in one data center (a profile pinned to a staged volume can only rent
    /// there), or across all of them.
    pub fn gpu_offers_in(
        &self,
        gpu_count: u32,
        data_center: Option<&str>,
    ) -> Result<Vec<GpuOffer>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Price {
            uninterruptable_price: Option<f64>,
            stock_status: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GpuType {
            id: String,
            #[serde(default)]
            display_name: String,
            #[serde(default)]
            memory_in_gb: u32,
            #[serde(default)]
            secure_cloud: bool,
            lowest_price: Option<Price>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            gpu_types: Vec<GpuType>,
        }
        let query = format!(
            "query {{ gpuTypes {{ id displayName memoryInGb secureCloud \
             lowestPrice(input: {{gpuCount: {gpu_count}, secureCloud: true{dc}}}) \
             {{ uninterruptablePrice stockStatus }} }} }}",
            dc = data_center
                .filter(|d| d.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
                .map(|d| format!(", dataCenterId: \"{d}\""))
                .unwrap_or_default()
        );
        let d: Data = self.graphql(&query, "gpu offers")?;
        let mut offers: Vec<GpuOffer> = d
            .gpu_types
            .into_iter()
            .filter(|g| g.secure_cloud && g.memory_in_gb > 0 && usable_gpu(&g.id))
            .map(|g| {
                let (price, stock) = match g.lowest_price {
                    Some(p) => (p.uninterruptable_price, p.stock_status),
                    None => (None, None),
                };
                GpuOffer {
                    id: g.id,
                    display_name: g.display_name,
                    memory_gb: g.memory_in_gb,
                    gpu_count,
                    price_per_hr: price,
                    stock,
                }
            })
            .collect();
        sort_offers(&mut offers);
        Ok(offers)
    }

    pub fn data_centers(&self) -> Result<Vec<DataCenter>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            data_centers: Vec<DataCenter>,
        }
        let d: Data = self.graphql(
            "query { dataCenters { id name storageSupport } }",
            "data centers",
        )?;
        Ok(d.data_centers)
    }
}

/// Whole NVIDIA GPUs only: the pinned Ollama image is the CUDA build, and a MIG slice
/// is a fraction of a card that cannot be combined into a multi-GPU pod.
pub fn usable_gpu(id: &str) -> bool {
    id.starts_with("NVIDIA ") && !id.contains(" MIG ")
}

/// Available offers first, by total price; unavailable ones after, by VRAM.
pub fn sort_offers(offers: &mut [GpuOffer]) {
    offers.sort_by(|a, b| match (a.price_per_hr, b.price_per_hr) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.memory_gb.cmp(&b.memory_gb),
    });
}

fn read_body(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<(u16, String)> {
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::http(what, e))?;
    if !(200..300).contains(&status) {
        let mut body = text;
        body.truncate(600);
        return Err(Error::Api {
            what: what.to_string(),
            status,
            body,
        });
    }
    Ok((status, text))
}

fn read_json<T: DeserializeOwned>(resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<T> {
    let (_, text) = read_body(resp, what)?;
    serde_json::from_str(&text).map_err(|e| Error::decode(what, e))
}

fn read_ok(resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<()> {
    read_body(resp, what).map(|_| ())
}

/// RunPod sends `costPerHr` as a number in practice and as a string in its schema examples.
fn lenient_f64<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
        Null(()),
    }
    Ok(match NumOrStr::deserialize(d)? {
        NumOrStr::Num(n) => n,
        NumOrStr::Str(s) => s.trim().parse().unwrap_or(0.0),
        NumOrStr::Null(()) => 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_parses_live_shape_and_finds_ssh_endpoint() {
        let json = r#"{"id":"cgf2ktkf2hnf7p","name":"zed-ssh-test","desiredStatus":"RUNNING",
            "imageName":"runpod/pytorch:2.4.0","publicIp":"213.173.102.150",
            "portMappings":{"22":15186},"ports":["22/tcp"],"costPerHr":0.24,"gpuCount":1,
            "lastStartedAt":"2026-10-02 16:11:40.1 +0000 UTC",
            "machine":{"gpuTypeId":"NVIDIA RTX 2000 Ada Generation","dataCenterId":"EU-RO-1"}}"#;
        let pod: Pod = serde_json::from_str(json).expect("live pod json should parse");
        assert!(pod.is_running());
        assert_eq!(pod.image, "runpod/pytorch:2.4.0");
        assert_eq!(
            pod.ssh_endpoint(),
            Some(("213.173.102.150".to_string(), 15186))
        );
        assert_eq!(pod.gpu_type(), Some("NVIDIA RTX 2000 Ada Generation"));
    }

    #[test]
    fn pod_without_tcp_mapping_has_no_endpoint() {
        let json = r#"{"id":"x","name":"ollama","desiredStatus":"RUNNING","publicIp":"",
            "portMappings":null,"ports":["11434/http"],"costPerHr":"0.79"}"#;
        let pod: Pod = serde_json::from_str(json).expect("pod json should parse");
        assert_eq!(pod.ssh_endpoint(), None);
        assert!(
            (pod.cost_per_hr - 0.79).abs() < 1e-9,
            "string cost should parse"
        );
    }

    #[test]
    fn create_body_uses_runpod_field_names_and_omits_unset() {
        let spec = PodCreate {
            name: "t".into(),
            image_name: "ollama/ollama:0.35.0".into(),
            gpu_type_ids: vec!["NVIDIA H200".into()],
            gpu_type_priority: "custom".into(),
            gpu_count: 2,
            cloud_type: "SECURE".into(),
            support_public_ip: true,
            ports: vec!["22/tcp".into()],
            container_disk_in_gb: 40,
            volume_in_gb: None,
            network_volume_id: Some("vol1".into()),
            volume_mount_path: "/workspace".into(),
            data_center_ids: vec![],
            allowed_cuda_versions: vec![],
            docker_entrypoint: vec!["bash".into(), "-c".into()],
            docker_start_cmd: vec!["echo".into()],
            env: BTreeMap::new(),
        };
        let v = serde_json::to_value(&spec).expect("spec should serialize");
        assert_eq!(v["imageName"], "ollama/ollama:0.35.0");
        assert_eq!(v["gpuTypeIds"][0], "NVIDIA H200");
        assert_eq!(v["networkVolumeId"], "vol1");
        assert!(v.get("volumeInGb").is_none());
        assert!(v.get("dataCenterIds").is_none());
        assert!(
            v.get("allowedCudaVersions").is_none(),
            "any host when unset"
        );
    }

    #[test]
    fn offers_sort_available_by_price_then_unavailable() {
        let mk = |id: &str, mem, price| GpuOffer {
            id: id.into(),
            display_name: id.into(),
            memory_gb: mem,
            gpu_count: 1,
            price_per_hr: price,
            stock: None,
        };
        let mut v = vec![
            mk("b", 141, None),
            mk("c", 80, Some(2.0)),
            mk("a", 48, Some(0.5)),
            mk("d", 80, None),
        ];
        sort_offers(&mut v);
        let ids: Vec<_> = v.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "d", "b"]);
    }

    #[test]
    fn only_whole_nvidia_gpus_are_offered() {
        assert!(usable_gpu("NVIDIA H200"));
        assert!(usable_gpu("NVIDIA RTX PRO 6000 Blackwell Server Edition"));
        assert!(!usable_gpu(
            "NVIDIA RTX PRO 6000 Blackwell Server Edition MIG 2g.48gb"
        ));
        assert!(!usable_gpu("AMD Instinct MI300X OAM"));
    }

    #[test]
    fn runway_counts_current_and_extra_spend() {
        let a = Account {
            client_balance: 13.6,
            current_spend_per_hr: 0.8,
            spend_limit: Some(80.0),
        };
        let h = a.runway_hours(2.6).expect("positive spend has a runway");
        assert!((h - 4.0).abs() < 1e-9);
        let idle = Account {
            client_balance: 5.0,
            current_spend_per_hr: 0.0,
            spend_limit: None,
        };
        assert_eq!(idle.runway_hours(0.0), None);
    }
}
