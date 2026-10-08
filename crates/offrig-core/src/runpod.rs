//! RunPod API client: REST v2 (`api.runpod.io/v2`) for pods and network volumes,
//! GraphQL (`api.runpod.io/graphql`) for GPU prices, stock and the account balance.
//! The v2 spec (read 2026-10-08) has no balance field. GraphQL stays for that
//! one query until it retires in early 2027.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};
use crate::trace;

pub const REST_BASE: &str = "https://api.runpod.io/v2";
pub const GRAPHQL_URL: &str = "https://api.runpod.io/graphql";

pub struct RunPod {
    agent: ureq::Agent,
    key: String,
    rest_base: String,
    graphql_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", try_from = "serde_json::Value")]
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

impl TryFrom<serde_json::Value> for Pod {
    type Error = serde_json::Error;

    fn try_from(v: serde_json::Value) -> std::result::Result<Self, Self::Error> {
        if v.get("status").is_some() && v.get("desiredStatus").is_none() {
            return Ok(pod_from_v2(&v));
        }
        let old: PodV1 = serde_json::from_value(v)?;
        Ok(Pod {
            id: old.id,
            name: old.name,
            desired_status: old.desired_status,
            image: old.image,
            public_ip: old.public_ip,
            port_mappings: old.port_mappings,
            ports: old.ports,
            cost_per_hr: old.cost_per_hr,
            gpu_count: old.gpu_count,
            last_started_at: old.last_started_at,
            network_volume_id: old.network_volume_id,
            machine: old.machine,
        })
    }
}

/// The v1 pod shape the fixtures still speak. v2 is mapped onto the same fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodV1 {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    desired_status: String,
    #[serde(default, alias = "imageName")]
    image: String,
    #[serde(default)]
    public_ip: Option<String>,
    #[serde(default)]
    port_mappings: Option<BTreeMap<String, u16>>,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cost_per_hr: f64,
    #[serde(default)]
    gpu_count: u32,
    #[serde(default)]
    last_started_at: Option<String>,
    #[serde(default)]
    network_volume_id: Option<String>,
    #[serde(default)]
    machine: Option<Machine>,
}

fn json_string(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn json_opt_string(v: &serde_json::Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn json_f64(v: &serde_json::Value, key: &str) -> f64 {
    match v.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// v2 `Pod` onto the fields the rest of offrig already reads. Direct SSH is copied
/// into `public_ip` and `port_mappings["22"]`. The proxy endpoint is not: it cannot
/// forward ports, and the tunnel needs a direct sshd.
fn pod_from_v2(v: &serde_json::Value) -> Pod {
    let gpu = v.get("gpu");
    let machine = Some(Machine {
        gpu_type_id: gpu
            .and_then(|g| g.get("id"))
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        data_center_id: json_opt_string(v, "dataCenterId"),
        cuda_version: json_opt_string(v, "cudaVersion"),
    });
    let direct = v
        .get("ssh")
        .and_then(|s| s.get("direct"))
        .filter(|d| !d.is_null());
    let (public_ip, port_mappings) = match direct {
        Some(d) => {
            let host = json_string(d, "host");
            let port = d.get("port").and_then(|p| p.as_u64()).unwrap_or(0);
            if host.is_empty() || port == 0 || port > u16::MAX as u64 {
                (None, None)
            } else {
                let mut map = BTreeMap::new();
                map.insert("22".to_string(), port as u16);
                (Some(host), Some(map))
            }
        }
        None => (None, None),
    };
    let network_volume_id = v
        .get("mounts")
        .and_then(|m| m.get("network"))
        .and_then(|n| n.as_array())
        .and_then(|a| a.first())
        .and_then(|n| n.get("volumeId"))
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let ports = v
        .get("ports")
        .and_then(|p| p.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Pod {
        id: json_string(v, "id"),
        name: json_string(v, "name"),
        desired_status: json_string(v, "status"),
        image: json_string(v, "image"),
        public_ip,
        port_mappings,
        ports,
        cost_per_hr: json_f64(v, "cost"),
        gpu_count: gpu
            .and_then(|g| g.get("count"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32,
        last_started_at: json_opt_string(v, "startedAt"),
        network_volume_id,
        machine,
    }
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

/// One pod the caller wants. `gpu_type_ids` is the plan's list, in plan order.
/// v2 create takes exactly one GPU type and does not fall back, so [`RunPod::create_pod`]
/// walks this list. The serialized body is the nested `CreatePodRequest` for the
/// first id; each attempt builds its own body with [`PodCreate::wire`].
#[derive(Debug, Clone, PartialEq)]
pub struct PodCreate {
    pub name: String,
    pub image_name: String,
    pub gpu_type_ids: Vec<String>,
    pub gpu_count: u32,
    pub cloud_type: String,
    /// Kept so a reader of the spec can see that direct SSH needs a published port.
    /// v2 has no `supportPublicIp`; publishing `22/tcp` is what makes `ssh.direct` appear.
    pub support_public_ip: bool,
    pub ports: Vec<String>,
    pub container_disk_in_gb: u32,
    pub volume_in_gb: Option<u32>,
    pub network_volume_id: Option<String>,
    pub volume_mount_path: String,
    pub data_center_ids: Vec<String>,
    /// Host CUDA floor (`major.minor`), from [`crate::config::Profile::effective_min_cuda`].
    /// Sent as `gpu.minCudaVersion`. `None` accepts any host.
    pub min_cuda_version: Option<String>,
    pub docker_entrypoint: Vec<String>,
    pub docker_start_cmd: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// When set, an attempt whose live offer is above this price is not sent.
    /// `None` on a profile launch that has no plan cap.
    pub max_price_hr: Option<f64>,
}

impl Serialize for PodCreate {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let id = self.gpu_type_ids.first().map(String::as_str).unwrap_or("");
        self.wire(id).serialize(serializer)
    }
}

impl PodCreate {
    /// The v2 `CreatePodRequest` for one GPU type. Nothing outside `gpu_type_ids` is added.
    pub fn wire(&self, gpu_id: &str) -> serde_json::Value {
        let mut gpu = serde_json::json!({
            "id": gpu_id,
            "count": self.gpu_count,
        });
        if let Some(version) = &self.min_cuda_version {
            gpu["minCudaVersion"] = serde_json::json!(version);
        }
        let mut body = serde_json::json!({
            "name": self.name,
            "image": self.image_name,
            "cloud": self.cloud_type,
            "ports": self.ports,
            "disk": self.container_disk_in_gb,
            "env": self.env,
            "entrypoint": self.docker_entrypoint,
            "cmd": self.docker_start_cmd,
            "gpu": gpu,
            // The bootstrap writes $PUBLIC_KEY into authorized_keys. v2 injects that
            // variable only when startSsh is set.
            "startSsh": true,
        });
        if !self.data_center_ids.is_empty() {
            body["dataCenterIds"] = serde_json::json!(self.data_center_ids);
        }
        if let Some(id) = &self.network_volume_id {
            body["mounts"] = serde_json::json!({
                "network": [{ "volumeId": id, "path": self.volume_mount_path }]
            });
        } else if let Some(size) = self.volume_in_gb {
            body["mounts"] = serde_json::json!({
                "persistent": { "size": size, "path": self.volume_mount_path }
            });
        }
        body
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkVolume {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u32,
    /// v2 calls this `dataCenter`. v1 called it `dataCenterId`.
    #[serde(default, alias = "dataCenter")]
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
        let mut pods = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let path = match &cursor {
                Some(c) => format!("/pods?cursor={}", query_escape(c)),
                None => "/pods".to_string(),
            };
            let started = std::time::Instant::now();
            let resp = self
                .agent
                .get(self.url(&path))
                .header("Authorization", self.auth())
                .call()
                .map_err(|e| Error::http("list pods", e))?;
            trace::api("list pods", resp.status().as_u16(), started);
            let text = read_text(resp, "list pods")?;
            let (page, has_next, next) = parse_pod_page(&text)?;
            pods.extend(page);
            if !has_next {
                break;
            }
            match next {
                Some(c) if Some(&c) != cursor.as_ref() => cursor = Some(c),
                _ => break,
            }
        }
        Ok(pods)
    }

    pub fn get_pod(&self, id: &str) -> Result<Pod> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .get(self.url(&format!("/pods/{id}")))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("get pod", e))?;
        trace::api("get pod", resp.status().as_u16(), started);
        read_json(resp, &format!("get pod {id}"))
    }

    /// Find a pod by exact id or exact name.
    pub fn find_pod(&self, id_or_name: &str) -> Result<Pod> {
        self.list_pods()?
            .into_iter()
            .find(|p| p.id == id_or_name || p.name == id_or_name)
            .ok_or_else(|| Error::PodNotFound(id_or_name.to_string()))
    }

    /// Create the pod, trying each GPU type in list order. A 400 means this type
    /// could not be placed; the next id is tried. Any other status stops the loop.
    /// An id whose live price is above `max_price_hr` is not sent. The list itself
    /// is never extended.
    pub fn create_pod(&self, spec: &PodCreate) -> Result<Pod> {
        if spec.gpu_type_ids.is_empty() {
            return Err(Error::NoCapacity(format!(
                "no GPU types listed for {}",
                spec.name
            )));
        }
        let offers = match spec.max_price_hr {
            Some(_) => self
                .gpu_offers_in(
                    spec.gpu_count,
                    spec.data_center_ids.first().map(String::as_str),
                )
                .ok(),
            None => None,
        };
        let mut tried = Vec::new();
        let mut last = String::new();
        for id in &spec.gpu_type_ids {
            if let (Some(cap), Some(offers)) = (spec.max_price_hr, offers.as_ref())
                && let Some(price) = offers
                    .iter()
                    .find(|o| o.id == *id)
                    .and_then(|o| o.price_per_hr)
                && price > cap + 1e-4
            {
                trace::verbose(&format!(
                    "runpod: skip {id} at ${price:.4}/hr; the plan cap is ${cap:.4}/hr"
                ));
                continue;
            }
            trace::verbose(&format!(
                "runpod: create {} on {}x {id}",
                spec.name, spec.gpu_count
            ));
            tried.push(id.as_str());
            match self.post_pod(&spec.wire(id), &spec.name) {
                Ok(pod) => return Ok(pod),
                Err(e) if placement_rejected(&e) => {
                    last = e.to_string();
                    trace::verbose(&format!("runpod: {id} was not placed"));
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::NoCapacity(if tried.is_empty() {
            format!(
                "every GPU listed for {} is above the plan price ${:.4}/hr",
                spec.name,
                spec.max_price_hr.unwrap_or(0.0)
            )
        } else {
            format!(
                "RunPod could not place {} on [{}]{}",
                spec.name,
                tried.join(" | "),
                if last.is_empty() {
                    String::new()
                } else {
                    format!(": {last}")
                }
            )
        }))
    }

    fn post_pod(&self, body: &serde_json::Value, name: &str) -> Result<Pod> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .post(self.url("/pods"))
            .header("Authorization", self.auth())
            .send_json(body)
            .map_err(|e| Error::http("create pod", e))?;
        trace::api("create pod", resp.status().as_u16(), started);
        read_json(resp, &format!("create pod {name}"))
    }

    /// Terminate a pod. Its container disk is gone afterwards; a network volume is not.
    pub fn delete_pod(&self, id: &str) -> Result<()> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .delete(self.url(&format!("/pods/{id}")))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("delete pod", e))?;
        trace::api("delete pod", resp.status().as_u16(), started);
        read_ok(resp, &format!("delete pod {id}"))
    }

    pub fn stop_pod(&self, id: &str) -> Result<()> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .post(self.url(&format!("/pods/{id}/action")))
            .header("Authorization", self.auth())
            .send_json(serde_json::json!({ "action": "stop" }))
            .map_err(|e| Error::http("stop pod", e))?;
        trace::api("stop pod", resp.status().as_u16(), started);
        read_ok(resp, &format!("stop pod {id}"))
    }

    pub fn list_volumes(&self) -> Result<Vec<NetworkVolume>> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .get(self.url("/network-volumes"))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("list network volumes", e))?;
        trace::api("list network volumes", resp.status().as_u16(), started);
        let text = read_text(resp, "list network volumes")?;
        parse_named_list(&text, "networkVolumes", "list network volumes")
    }

    pub fn create_volume(
        &self,
        name: &str,
        size_gb: u32,
        data_center_id: &str,
    ) -> Result<NetworkVolume> {
        let body =
            serde_json::json!({ "name": name, "size": size_gb, "dataCenter": data_center_id });
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .post(self.url("/network-volumes"))
            .header("Authorization", self.auth())
            .send_json(&body)
            .map_err(|e| Error::http("create network volume", e))?;
        trace::api("create network volume", resp.status().as_u16(), started);
        read_json(resp, &format!("create network volume {name}"))
    }

    pub fn delete_volume(&self, id: &str) -> Result<()> {
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .delete(self.url(&format!("/network-volumes/{id}")))
            .header("Authorization", self.auth())
            .call()
            .map_err(|e| Error::http("delete network volume", e))?;
        trace::api("delete network volume", resp.status().as_u16(), started);
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
        let started = std::time::Instant::now();
        let resp = self
            .agent
            .post(&self.graphql_url)
            .header("Authorization", self.auth())
            .send_json(serde_json::json!({ "query": query }))
            .map_err(|e| Error::http(what, e))?;
        trace::api(what, resp.status().as_u16(), started);
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

/// v2 create returns 400 when that GPU type cannot be placed. 422 is a bad body
/// and must not move the loop on.
fn placement_rejected(e: &Error) -> bool {
    matches!(e, Error::Api { status: 400, .. })
}

fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn read_body(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<(u16, String)> {
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::http(what, e))?;
    if !(200..300).contains(&status) {
        trace::debug(&format!("runpod: {what} failed, response body: {text}"));
        return Err(Error::Api {
            what: what.to_string(),
            status,
            body: problem_text(&text),
        });
    }
    Ok((status, text))
}

fn read_text(resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<String> {
    read_body(resp, what).map(|(_, text)| text)
}

/// RFC 9457 `title`, `detail` and `errors`, so the detail survives in [`Error::Api`].
/// A body that is not a problem document is kept as-is, clipped to 600 bytes.
fn problem_text(text: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return clip(text);
    };
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let detail = v.get("detail").and_then(|t| t.as_str()).unwrap_or("");
    if title.is_empty() && detail.is_empty() && v.get("errors").is_none() {
        return clip(text);
    }
    let mut s = match (title.is_empty(), detail.is_empty()) {
        (true, true) => String::new(),
        (false, true) => title.to_string(),
        (true, false) => detail.to_string(),
        (false, false) => format!("{title}: {detail}"),
    };
    if let Some(errors) = v.get("errors").and_then(|e| e.as_array()) {
        let extra: Vec<&str> = errors.iter().filter_map(|e| e.as_str()).collect();
        if !extra.is_empty() {
            if !s.is_empty() {
                s.push_str("; ");
            }
            s.push_str(&extra.join("; "));
        }
    }
    clip(&s)
}

fn clip(text: &str) -> String {
    let mut body = text.to_string();
    body.truncate(600);
    body
}

fn parse_pod_page(text: &str) -> Result<(Vec<Pod>, bool, Option<String>)> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| Error::decode("list pods", e))?;
    if let Some(arr) = v.as_array() {
        let pods: Vec<Pod> = serde_json::from_value(serde_json::Value::Array(arr.clone()))
            .map_err(|e| Error::decode("list pods", e))?;
        return Ok((pods, false, None));
    }
    let pods = parse_named_list(text, "pods", "list pods")?;
    let page = v.get("pagination");
    let has_next = page
        .and_then(|p| p.get("hasNextPage"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let next = page
        .and_then(|p| p.get("nextCursor"))
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Ok((pods, has_next, next))
}

fn parse_named_list<T: DeserializeOwned>(text: &str, key: &str, what: &str) -> Result<Vec<T>> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| Error::decode(what, e))?;
    let arr = if let Some(arr) = v.as_array() {
        serde_json::Value::Array(arr.clone())
    } else {
        v.get(key).cloned().ok_or_else(|| {
            Error::decode(
                what,
                <serde_json::Error as serde::de::Error>::custom(format!("missing {key}")),
            )
        })?
    };
    serde_json::from_value(arr).map_err(|e| Error::decode(what, e))
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
    fn create_body_is_a_nested_v2_request_and_omits_unset() {
        let spec = PodCreate {
            name: "t".into(),
            image_name: "ollama/ollama:0.35.0".into(),
            gpu_type_ids: vec!["NVIDIA H200".into()],
            gpu_count: 2,
            cloud_type: "SECURE".into(),
            support_public_ip: true,
            ports: vec!["22/tcp".into()],
            container_disk_in_gb: 40,
            volume_in_gb: None,
            network_volume_id: Some("vol1".into()),
            volume_mount_path: "/workspace".into(),
            data_center_ids: vec![],
            min_cuda_version: None,
            docker_entrypoint: vec!["bash".into(), "-c".into()],
            docker_start_cmd: vec!["echo".into()],
            env: BTreeMap::new(),
            max_price_hr: None,
        };
        let v = serde_json::to_value(&spec).expect("spec should serialize");
        assert_eq!(v["image"], "ollama/ollama:0.35.0");
        assert_eq!(v["gpu"]["id"], "NVIDIA H200");
        assert_eq!(v["gpu"]["count"], 2);
        assert_eq!(v["cloud"], "SECURE");
        assert_eq!(v["startSsh"], true);
        assert_eq!(v["disk"], 40);
        assert_eq!(v["mounts"]["network"][0]["volumeId"], "vol1");
        assert!(v["mounts"].get("persistent").is_none());
        assert!(v.get("dataCenterIds").is_none());
        assert!(
            v["gpu"].get("minCudaVersion").is_none(),
            "any host when unset"
        );
        assert!(v.get("gpuTypeIds").is_none());
        assert!(v.get("interruptible").is_none());
        assert!(v.get("minDownloadMbps").is_none());
    }

    #[test]
    fn a_v2_pod_uses_direct_ssh_and_ignores_the_proxy() {
        let json = r#"{"id":"pod1","name":"offrig-small","status":"RUNNING",
            "image":"ollama/ollama:0.35.0","cost":0.24,"ports":["22/tcp"],
            "cudaVersion":"12.8","dataCenterId":"EU-RO-1",
            "gpu":{"id":"NVIDIA RTX 2000 Ada Generation","count":1},
            "ssh":{
              "proxy":{"host":"ssh.runpod.io","port":22,"username":"tok","command":"ssh tok@ssh.runpod.io"},
              "direct":{"host":"203.0.113.10","port":15186,"username":"root","command":"ssh root@203.0.113.10"}
            },
            "mounts":{"network":[{"volumeId":"vol1","path":"/workspace"}]}}"#;
        let pod: Pod = serde_json::from_str(json).expect("v2 pod");
        assert!(pod.is_running());
        assert_eq!(pod.image, "ollama/ollama:0.35.0");
        assert_eq!(
            pod.ssh_endpoint(),
            Some(("203.0.113.10".to_string(), 15186))
        );
        assert_eq!(pod.gpu_type(), Some("NVIDIA RTX 2000 Ada Generation"));
        assert_eq!(pod.host_cuda(), Some("12.8"));
        assert_eq!(pod.gpu_count, 1);
        assert_eq!(pod.network_volume_id.as_deref(), Some("vol1"));
        assert!((pod.cost_per_hr - 0.24).abs() < 1e-9);
    }

    #[test]
    fn a_v2_pod_without_direct_ssh_has_no_endpoint() {
        let json = r#"{"id":"pod1","name":"offrig-small","status":"RUNNING","cost":0,
            "ssh":{"proxy":{"host":"ssh.runpod.io","port":22,"username":"tok","command":"ssh"},"direct":null}}"#;
        let pod: Pod = serde_json::from_str(json).expect("v2 pod");
        assert_eq!(pod.ssh_endpoint(), None);
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
