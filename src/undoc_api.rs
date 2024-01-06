use crate::cache::{cache_get, CacheComputeResult, CacheGetOptions};
use crate::lan_api::boolean_int;
use crate::opt_env_var;
use crate::platform_api::{
    http_response_body, DeviceCapability, DeviceCapabilityKind, DeviceParameters, EnumOption,
};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

// <https://github.com/constructorfleet/homebridge-ultimate-govee/blob/main/src/data/clients/RestClient.ts>

const APP_VERSION: &str = "5.6.01";
const HALF_DAY: Duration = Duration::from_secs(3600 * 12);
const ONE_DAY: Duration = Duration::from_secs(86400);
const ONE_WEEK: Duration = Duration::from_secs(86400 * 7);

fn user_agent() -> String {
    format!(
        "GoveeHome/{APP_VERSION} (com.ihoment.GoVeeSensor; build:2; iOS 16.5.0) Alamofire/5.6.4"
    )
}

pub fn ms_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix epoch in the past")
        .as_millis()
        .to_string()
}

#[derive(clap::Parser, Debug)]
pub struct UndocApiArguments {
    /// The email address you registered with Govee.
    /// If not passed here, it will be read from
    /// the GOVEE_EMAIL environment variable.
    #[arg(long, global = true)]
    pub govee_email: Option<String>,

    /// The password for your Govee account.
    /// If not passed here, it will be read from
    /// the GOVEE_PASSWORD environment variable.
    #[arg(long, global = true)]
    pub govee_password: Option<String>,

    /// Where to store the AWS IoT key file.
    #[arg(long, global = true, default_value = "/dev/shm/govee.iot.key")]
    pub govee_iot_key: PathBuf,

    /// Where to store the AWS IoT certificate file.
    #[arg(long, global = true, default_value = "/dev/shm/govee.iot.cert")]
    pub govee_iot_cert: PathBuf,

    /// Where to find the AWS root CA certificate
    #[arg(long, global = true, default_value = "AmazonRootCA1.pem")]
    pub amazon_root_ca: PathBuf,
}

impl UndocApiArguments {
    pub fn opt_email(&self) -> anyhow::Result<Option<String>> {
        match &self.govee_email {
            Some(key) => Ok(Some(key.to_string())),
            None => opt_env_var("GOVEE_EMAIL"),
        }
    }

    pub fn email(&self) -> anyhow::Result<String> {
        self.opt_email()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the govee account email either via the \
                --govee-email parameter or by setting $GOVEE_EMAIL"
            )
        })
    }

    pub fn opt_password(&self) -> anyhow::Result<Option<String>> {
        match &self.govee_password {
            Some(key) => Ok(Some(key.to_string())),
            None => opt_env_var("GOVEE_PASSWORD"),
        }
    }

    pub fn password(&self) -> anyhow::Result<String> {
        self.opt_password()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the govee account password either via the \
                --govee-password parameter or by setting $GOVEE_PASSWORD"
            )
        })
    }

    pub fn api_client(&self) -> anyhow::Result<GoveeUndocumentedApi> {
        let email = self.email()?;
        let password = self.password()?;
        Ok(GoveeUndocumentedApi::new(email, password))
    }
}

#[derive(Clone)]
pub struct GoveeUndocumentedApi {
    email: String,
    password: String,
    client_id: String,
}

impl GoveeUndocumentedApi {
    pub fn new<E: Into<String>, P: Into<String>>(email: E, password: P) -> Self {
        let email = email.into();
        let password = password.into();
        let client_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, email.as_bytes());
        let client_id = format!("{}", client_id.simple());
        Self {
            email,
            password,
            client_id,
        }
    }

    #[allow(unused)]
    pub async fn get_iot_key(&self, token: &str) -> anyhow::Result<IotKey> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "iot-key",
                soft_ttl: HALF_DAY,
                hard_ttl: HALF_DAY,
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()?
                    .request(Method::GET, "https://app2.govee.com/app/v1/account/iot/key")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("appVersion", APP_VERSION)
                    .header("clientId", &self.client_id)
                    .header("clientType", "1")
                    .header("iotVersion", "0")
                    .header("timestamp", ms_timestamp())
                    .header("User-Agent", user_agent())
                    .send()
                    .await?;

                #[derive(Deserialize, Debug)]
                #[allow(non_snake_case, dead_code)]
                struct Response {
                    data: IotKey,
                    message: String,
                    status: u64,
                }

                let resp: Response = http_response_body(response).await?;

                Ok(CacheComputeResult::Value(resp.data))
            },
        )
        .await
    }

    pub fn invalidate_account_login(&self) {
        crate::cache::invalidate_key("undoc-api", "account-info").ok();
    }

    async fn login_account_impl(&self) -> anyhow::Result<CacheComputeResult<LoginAccountResponse>> {
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .request(
                Method::POST,
                "https://app2.govee.com/account/rest/account/v1/login",
            )
            .json(&serde_json::json!({
                "email": self.email,
                "password": self.password,
                "client": &self.client_id,
            }))
            .send()
            .await?;

        let resp: Response = http_response_body(response).await?;

        #[derive(Deserialize, Serialize, Debug)]
        #[allow(non_snake_case, dead_code)]
        struct Response {
            client: LoginAccountResponse,
            message: String,
            status: u64,
        }

        let ttl = Duration::from_secs(resp.client.token_expire_cycle as u64);
        Ok(CacheComputeResult::WithTtl(resp.client, ttl))
    }

    pub async fn login_account_cached(&self) -> anyhow::Result<LoginAccountResponse> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "account-info",
                soft_ttl: HALF_DAY,
                hard_ttl: HALF_DAY,
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async { self.login_account_impl().await },
        )
        .await
    }

    pub async fn login_account(&self) -> anyhow::Result<LoginAccountResponse> {
        let value = self.login_account_impl().await?;
        Ok(value.into_inner())
    }

    pub async fn get_device_list(&self, token: &str) -> anyhow::Result<DevicesResponse> {
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .request(
                Method::POST,
                "https://app2.govee.com/device/rest/devices/v1/list",
            )
            .header("Authorization", format!("Bearer {token}"))
            .header("appVersion", APP_VERSION)
            .header("clientId", &self.client_id)
            .header("clientType", "1")
            .header("iotVersion", "0")
            .header("timestamp", ms_timestamp())
            .header("User-Agent", user_agent())
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.invalidate_account_login();
        }

        let resp: DevicesResponse = http_response_body(response).await?;

        Ok(resp)
    }

    pub fn invalidate_community_login(&self) {
        crate::cache::invalidate_key("undoc-api", "community-login").ok();
    }

    /// Login to community-api.govee.com and return the bearer token
    pub async fn login_community(&self) -> anyhow::Result<String> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "community-login",
                soft_ttl: ONE_DAY,
                hard_ttl: HALF_DAY,
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .build()?
                    .request(Method::POST, "https://community-api.govee.com/os/v1/login")
                    .json(&serde_json::json!({
                        "email": self.email,
                        "password": self.password,
                    }))
                    .send()
                    .await?;

                #[derive(Deserialize, Debug)]
                #[allow(non_snake_case, dead_code)]
                struct Response {
                    data: ResponseData,
                    message: String,
                    status: u64,
                }

                #[derive(Deserialize, Debug)]
                #[allow(non_snake_case, dead_code)]
                struct ResponseData {
                    email: String,
                    expiredAt: u64,
                    headerUrl: String,
                    id: u64,
                    nickName: String,
                    token: String,
                }

                let resp: Response = http_response_body(response).await?;

                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("unix epoch in the past")
                    .as_millis();

                let ttl_ms = resp.data.expiredAt as u128 - ts_ms;
                let ttl = Duration::from_millis(ttl_ms as u64).min(ONE_DAY);

                Ok(CacheComputeResult::WithTtl(resp.data.token, ttl))
            },
        )
        .await
    }

    pub async fn get_scenes_for_device(sku: &str) -> anyhow::Result<Vec<LightEffectCategory>> {
        let key = format!("scenes-{sku}");

        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: &key,
                soft_ttl: ONE_DAY,
                hard_ttl: ONE_WEEK,
                negative_ttl: Duration::from_secs(1),
                allow_stale: true,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?
                    .request(
                        Method::GET,
                        format!(
                            "https://app2.govee.com/appsku/v1/light-effect-libraries?sku={sku}"
                        ),
                    )
                    .header("AppVersion", APP_VERSION)
                    .header("User-Agent", user_agent())
                    .send()
                    .await?;

                let resp: LightEffectLibraryResponse = http_response_body(response).await?;

                Ok(CacheComputeResult::Value(resp.data.categories))
            },
        )
        .await
    }

    /// This is present primarily to workaround a bug where Govee aren't returning
    /// the full list of scenes via their supported platform API
    pub async fn synthesize_platform_api_scene_list(
        sku: &str,
    ) -> anyhow::Result<Vec<DeviceCapability>> {
        let catalog = Self::get_scenes_for_device(sku).await?;
        let mut options = vec![];

        for c in catalog {
            for s in c.scenes {
                if let Some(param_id) = s.light_effects.get(0).map(|e| e.scence_param_id) {
                    options.push(EnumOption {
                        name: s.scene_name,
                        value: json!({
                            "paramId": param_id,
                            "id": s.scene_id,
                        }),
                        extras: Default::default(),
                    });
                }
            }
        }

        Ok(vec![DeviceCapability {
            kind: DeviceCapabilityKind::DynamicScene,
            parameters: Some(DeviceParameters::Enum { options }),
            alarm_type: None,
            event_state: None,
            instance: "lightScene".to_string(),
        }])
    }

    pub async fn get_saved_one_click_shortcuts(
        &self,
        community_token: &str,
    ) -> anyhow::Result<Vec<OneClickComponent>> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "one-click-shortcuts",
                soft_ttl: ONE_DAY,
                hard_ttl: ONE_WEEK,
                negative_ttl: Duration::from_secs(1),
                allow_stale: true,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?
                    .request(
                        Method::GET,
                        "https://app2.govee.com/bff-app/v1/exec-plat/home",
                    )
                    .header("Authorization", format!("Bearer {community_token}"))
                    .header("appVersion", APP_VERSION)
                    .header("clientId", &self.client_id)
                    .header("clientType", "1")
                    .header("iotVersion", "0")
                    .header("timestamp", ms_timestamp())
                    .header("User-Agent", user_agent())
                    .send()
                    .await?;

                if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                    self.invalidate_community_login();
                }

                let resp: OneClickResponse = http_response_body(response).await?;

                Ok(CacheComputeResult::Value(resp.data.components))
            },
        )
        .await
    }

    pub async fn parse_one_clicks(&self) -> anyhow::Result<Vec<ParsedOneClick>> {
        let token = self.login_community().await?;
        let res = self.get_saved_one_click_shortcuts(&token).await?;
        let mut result = vec![];

        for group in res {
            for oc in group.one_clicks {
                if oc.iot_rules.is_empty() {
                    continue;
                }

                let name = format!("One-Click: {}: {}", group.name, oc.name);

                let mut entries = vec![];
                for rule in oc.iot_rules {
                    let msgs = rule.rule.into_iter().map(|r| r.iot_msg).collect();
                    entries.push(ParsedOneClickEntry {
                        topic: rule.device_obj.topic,
                        device: rule.device_obj.device,
                        msgs,
                    });
                }

                result.push(ParsedOneClick { name, entries });
            }
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedOneClick {
    pub name: String,
    pub entries: Vec<ParsedOneClickEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedOneClickEntry {
    pub topic: String,
    pub device: String,
    pub msgs: Vec<JsonValue>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
#[serde(rename_all = "camelCase")]
pub struct IotKey {
    pub endpoint: String,
    pub log: String,
    pub p12: String,
    pub p12_pass: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectLibraryResponse {
    pub data: LightEffectLibraryCategoryList,
    pub message: String,
    pub status: u32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectLibraryCategoryList {
    pub categories: Vec<LightEffectCategory>,
    pub support_speed: u8,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectCategory {
    pub category_id: u32,
    pub category_name: String,
    pub scenes: Vec<LightEffectScene>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectScene {
    pub scene_id: u32,
    pub icon_urls: Vec<String>,
    pub scene_name: String,
    pub analytic_name: String,
    pub scene_type: u32,
    pub scene_code: u32,
    pub scence_category_id: u32,
    pub pop_up_prompt: u32,
    pub scenes_hint: String,
    /// Eg: min/max applicable device version constraints
    pub rule: JsonValue,
    pub light_effects: Vec<LightEffectEntry>,
    pub voice_url: String,
    pub create_time: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectEntry {
    pub scence_param_id: u32,
    pub scence_name: String,
    /// base64 encoded
    pub scence_param: String,
    pub scene_code: u16,
    pub special_effect: Vec<JsonValue>,
    pub cmd_version: u32,
    pub scene_type: u32,
    pub diy_effect_code: Vec<JsonValue>,
    pub diy_effect_str: String,
    pub rules: Vec<JsonValue>,
    pub speed_info: JsonValue,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickResponse {
    pub data: OneClickComponentList,
    pub message: String,
    pub status: u32,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickComponentList {
    pub components: Vec<OneClickComponent>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickComponent {
    pub can_disable: Option<u8>,
    #[serde(deserialize_with = "boolean_int")]
    pub can_manage: bool,

    pub feast_type: Option<u64>,
    #[serde(default)]
    pub feasts: Vec<JsonValue>,

    #[serde(default)]
    pub groups: Vec<JsonValue>,

    pub main_device: Option<JsonValue>,

    pub component_id: u64,
    #[serde(default)]
    pub environments: Vec<JsonValue>,
    pub name: String,
    #[serde(rename = "type")]
    pub component_type: u64,

    pub guide_url: Option<String>,
    pub h5_url: Option<String>,
    pub video_url: Option<String>,

    #[serde(default)]
    pub one_clicks: Vec<OneClick>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClick {
    pub name: String,
    pub plan_type: u32,
    pub preset_id: u32,
    pub preset_state: u32,
    pub siri_engine_id: u32,
    #[serde(rename = "type")]
    pub rule_type: u32,
    pub desc: String,
    #[serde(default)]
    pub exec_rules: Vec<JsonValue>,
    pub group_id: u64,
    pub group_name: String,
    #[serde(default)]
    pub iot_rules: Vec<OneClickIotRule>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRule {
    pub device_obj: OneClickIotRuleDevice,
    pub rule: Vec<OneClickIotRuleEntry>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRuleEntry {
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub blue_msg: JsonValue,
    pub cmd_type: u64,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub cmd_val: OneClickIotRuleEntryCmd,
    pub device_type: u32,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub iot_msg: JsonValue,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRuleEntryCmd {
    pub open: Option<u32>,
    pub scenes_code: Option<u16>,
    pub scence_id: Option<u16>,
    pub scenes_str: Option<String>,
    pub scence_param_id: Option<u16>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRuleDevice {
    pub name: String,
    pub device: String,
    pub sku: String,

    pub topic: String,

    pub ble_address: String,
    pub ble_name: String,
    pub device_splicing_status: u32,
    pub feast_id: u64,
    pub feast_name: String,
    pub feast_type: u64,
    pub goods_type: u64,
    pub ic: Option<u32>,
    #[serde(rename = "ic_sub_1")]
    pub ic_sub_1: Option<u32>,
    #[serde(rename = "ic_sub_2")]
    pub ic_sub_2: Option<u32>,
    #[serde(deserialize_with = "boolean_int")]
    pub is_feast: bool,
    pub pact_type: u32,
    pub pact_code: u32,

    pub settings: Option<JsonValue>,
    pub spec: String,
    pub sub_device: String,
    pub sub_device_num: u64,
    pub sub_devices: Option<JsonValue>,

    pub version_hard: String,
    pub version_soft: String,
    pub wifi_soft_version: String,
    pub wifi_hard_version: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LoginAccountResponse {
    #[serde(rename = "A")]
    pub a: String,
    #[serde(rename = "B")]
    pub b: String,
    pub account_id: u64,
    /// this is the client id that we passed in
    pub client: String,
    pub is_savvy_user: bool,
    pub refresh_token: Option<String>,
    pub client_name: Option<String>,
    pub push_token: Option<String>,
    pub version_code: Option<String>,
    pub version_name: Option<String>,
    pub sys_version: Option<String>,
    pub token: String,
    pub token_expire_cycle: u32,
    pub topic: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DevicesResponse {
    pub devices: Vec<DeviceEntry>,
    pub groups: Vec<GroupEntry>,
    pub message: String,
    pub status: u16,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GroupEntry {
    pub group_id: u64,
    pub group_name: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceEntry {
    pub attributes_id: u32,
    pub device_id: Option<u32>,
    pub device: String,
    pub device_ext: DeviceEntryExt,
    pub device_name: String,
    pub goods_type: u32,
    pub group_id: u64,
    pub pact_code: Option<u32>,
    pub pact_type: Option<u32>,
    pub share: Option<u32>,
    pub sku: String,
    pub spec: String,
    #[serde(deserialize_with = "boolean_int")]
    pub support_scene: bool,
    pub version_hard: String,
    pub version_soft: String,
}
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceEntryExt {
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub device_settings: DeviceSettings,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub ext_resources: ExtResources,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub last_device_data: LastDeviceData,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceSettings {
    /// Maybe be absent for BLE devices
    pub wifi_name: Option<String>,
    pub address: Option<String>,
    pub ble_name: String,
    pub topic: Option<String>,
    pub wifi_mac: Option<String>,
    pub pact_type: Option<u32>,
    pub pact_code: Option<u32>,
    pub dsp_version_soft: Option<JsonValue>,
    pub wifi_soft_version: Option<String>,
    pub wifi_hard_version: Option<String>,
    pub ic: Option<u32>,
    #[serde(rename = "ic_sub_1")]
    pub ic_sub_1: Option<u32>,
    #[serde(rename = "ic_sub_2")]
    pub ic_sub_2: Option<u32>,
    pub secret_code: Option<String>,
    #[serde(deserialize_with = "boolean_int")]
    pub boil_water_completed_noti_on_off: bool,
    #[serde(deserialize_with = "boolean_int")]
    pub completion_noti_on_off: bool,
    #[serde(deserialize_with = "boolean_int")]
    pub auto_shut_down_on_off: bool,
    pub sku: String,
    pub device: String,
    pub device_name: String,
    pub version_hard: String,
    pub version_soft: String,
    pub play_state: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct ExtResources {
    pub sku_url: String,
    pub head_on_img_new: Option<String>,
    pub head_on_img: String,
    pub head_off_img: String,
    pub head_off_img_new: Option<String>,
    pub ext: String,
    pub ic: u32,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LastDeviceData {
    pub online: bool,
}

pub fn as_json<S, T>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: serde::Serializer,
{
    use serde::ser::Error as _;

    let s = serde_json::to_string(value).map_err(|e| S::Error::custom(format!("{e:#}")))?;

    s.serialize(serializer)
}

pub fn embedded_json<'de, T: DeserializeOwned, D: serde::de::Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    use serde::de::Error as _;
    let s = String::deserialize(deserializer)?;
    serde_json::from_str(&s).map_err(|e| {
        D::Error::custom(format!(
            "{} {e:#} while processing embedded json text {s}",
            std::any::type_name::<T>()
        ))
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::platform_api::from_json;

    #[test]
    fn get_device_scenes() {
        let resp: DevicesResponse =
            from_json(include_str!("../test-data/undoc-device-list.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn get_one_click() {
        let resp: OneClickResponse =
            from_json(include_str!("../test-data/undoc-one-click.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn light_effect_library() {
        let resp: LightEffectLibraryResponse =
            from_json(include_str!("../test-data/light-effect-library-h6072.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn issue_14() {
        let resp: DevicesResponse = from_json(include_str!("../test-data/issue14.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }
}
