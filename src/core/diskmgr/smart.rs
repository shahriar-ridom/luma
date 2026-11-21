//! Reading `smartctl --json` output. Pure parsing: no commands are run here.

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Good,
    /// Still working, but worth watching or backing up.
    Warning,
    /// The drive reports that it is failing.
    Failing,
    /// The drive did not say.
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AtaStats {
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub uncorrectable: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NvmeStats {
    pub critical_warning: u64,
    pub percentage_used: Option<u64>,
    pub available_spare: Option<u64>,
    pub spare_threshold: Option<u64>,
    pub media_errors: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfTest {
    pub kind: String,
    pub result: String,
    pub passed: bool,
    pub hours: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmartReport {
    pub model: String,
    pub passed: Option<bool>,
    pub temp_c: Option<i64>,
    pub power_on_hours: Option<u64>,
    pub ata: Option<AtaStats>,
    pub nvme: Option<NvmeStats>,
    pub last_test: Option<SelfTest>,
    /// A self-test is running: percent left.
    pub test_remaining: Option<u64>,
}

impl SmartReport {
    /// Overall judgement plus the reasons for it, in plain words.
    pub fn verdict(&self) -> (Health, Vec<String>) {
        let mut bad = vec![];
        let mut warn = vec![];
        if self.passed == Some(false) {
            bad.push("The drive reports that it is failing its own health check.".to_string());
        }
        if let Some(n) = &self.nvme {
            if n.critical_warning != 0 {
                bad.push(format!("The drive raised a critical warning (flags {:#04x}).", n.critical_warning));
            }
            if let Some(u) = n.percentage_used.filter(|&u| u >= 90) {
                warn.push(format!("{u}% of the rated lifetime is used."));
            }
            if let (Some(s), Some(t)) = (n.available_spare, n.spare_threshold)
                && s <= t
            {
                warn.push(format!("Spare capacity is down to {s}%."));
            }
            if let Some(m) = n.media_errors.filter(|&m| m > 0) {
                warn.push(format!("{m} media or data errors recorded."));
            }
        }
        if let Some(a) = &self.ata {
            for (n, what) in [
                (a.reallocated, "reallocated sectors"),
                (a.pending, "sectors waiting to be reallocated"),
                (a.uncorrectable, "uncorrectable sectors"),
            ] {
                if let Some(n) = n.filter(|&n| n > 0) {
                    warn.push(format!("{n} {what}."));
                }
            }
        }
        if let Some(t) = &self.last_test
            && !t.passed
        {
            warn.push(format!("The last self-test did not pass ({}).", t.result.to_lowercase()));
        }
        if let Some(t) = self.temp_c.filter(|&t| t >= 70) {
            warn.push(format!("Running hot: {t} °C."));
        }
        let health = if !bad.is_empty() {
            Health::Failing
        } else if !warn.is_empty() {
            Health::Warning
        } else if self.passed == Some(true) {
            Health::Good
        } else {
            Health::Unknown
        };
        bad.extend(warn);
        (health, bad)
    }
}

#[derive(Deserialize)]
struct Raw {
    smartctl: Option<Meta>,
    model_name: Option<String>,
    smart_status: Option<Status>,
    temperature: Option<Temp>,
    power_on_time: Option<Hours>,
    ata_smart_attributes: Option<Attrs>,
    ata_smart_data: Option<AtaData>,
    ata_smart_self_test_log: Option<TestLog>,
    nvme_smart_health_information_log: Option<NvmeLog>,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    string: Option<String>,
    severity: Option<String>,
}

#[derive(Deserialize)]
struct Status {
    passed: Option<bool>,
}

#[derive(Deserialize)]
struct Temp {
    current: Option<i64>,
}

#[derive(Deserialize)]
struct Hours {
    hours: Option<u64>,
}

#[derive(Deserialize)]
struct Attrs {
    #[serde(default)]
    table: Vec<Attr>,
}

#[derive(Deserialize)]
struct Attr {
    id: u64,
    raw: Option<AttrRaw>,
}

#[derive(Deserialize)]
struct AttrRaw {
    value: Option<u64>,
}

#[derive(Deserialize)]
struct AtaData {
    self_test: Option<AtaSelfTest>,
}

#[derive(Deserialize)]
struct AtaSelfTest {
    status: Option<TestStatus>,
}

#[derive(Deserialize)]
struct TestStatus {
    remaining_percent: Option<u64>,
}

#[derive(Deserialize)]
struct TestLog {
    standard: Option<Standard>,
}

#[derive(Deserialize)]
struct Standard {
    #[serde(default)]
    table: Vec<TestRow>,
}

#[derive(Deserialize)]
struct TestRow {
    #[serde(rename = "type")]
    kind: Option<Named>,
    status: Option<RowStatus>,
    lifetime_hours: Option<u64>,
}

#[derive(Deserialize)]
struct Named {
    string: Option<String>,
}

#[derive(Deserialize)]
struct RowStatus {
    string: Option<String>,
    passed: Option<bool>,
}

#[derive(Deserialize)]
struct NvmeLog {
    critical_warning: Option<u64>,
    temperature: Option<i64>,
    available_spare: Option<u64>,
    available_spare_threshold: Option<u64>,
    percentage_used: Option<u64>,
    media_errors: Option<u64>,
    power_on_hours: Option<u64>,
    unsafe_shutdowns: Option<u64>,
}

/// Parse the JSON `smartctl` prints. A drive that could not be read gives an
/// error with smartctl's own explanation; a drive that answered only in part
/// gives a report with the missing fields empty.
pub fn parse(json: &str) -> Result<SmartReport, String> {
    let raw: Raw = serde_json::from_str(json).map_err(|e| format!("unreadable smartctl output: {e}"))?;
    let has_data = raw.smart_status.is_some()
        || raw.ata_smart_attributes.is_some()
        || raw.nvme_smart_health_information_log.is_some();
    if !has_data {
        let why = raw
            .smartctl
            .iter()
            .flat_map(|m| &m.messages)
            .filter(|m| m.severity.as_deref() == Some("error"))
            .filter_map(|m| m.string.clone())
            .collect::<Vec<_>>()
            .join(" ");
        return Err(if why.is_empty() { "The drive did not report any health data.".into() } else { why });
    }

    let attr = |id: u64| {
        raw.ata_smart_attributes.as_ref()?.table.iter().find(|a| a.id == id).and_then(|a| a.raw.as_ref()?.value)
    };
    let nvme = raw.nvme_smart_health_information_log.as_ref();
    let ata = raw.ata_smart_attributes.is_some().then(|| AtaStats {
        reallocated: attr(5),
        pending: attr(197),
        uncorrectable: attr(198),
    });
    let status = raw.ata_smart_data.as_ref().and_then(|d| d.self_test.as_ref()?.status.as_ref());
    let last_test =
        raw.ata_smart_self_test_log.as_ref().and_then(|l| l.standard.as_ref()?.table.first()).and_then(|r| {
            let s = r.status.as_ref()?;
            Some(SelfTest {
                kind: r.kind.as_ref()?.string.clone().unwrap_or_default(),
                result: s.string.clone().unwrap_or_default(),
                passed: s.passed?,
                hours: r.lifetime_hours,
            })
        });

    Ok(SmartReport {
        model: raw.model_name.clone().unwrap_or_default(),
        passed: raw.smart_status.as_ref().and_then(|s| s.passed),
        temp_c: raw.temperature.as_ref().and_then(|t| t.current).or(nvme.and_then(|n| n.temperature)),
        power_on_hours: raw.power_on_time.as_ref().and_then(|p| p.hours).or(nvme.and_then(|n| n.power_on_hours)),
        ata,
        nvme: nvme.map(|n| NvmeStats {
            critical_warning: n.critical_warning.unwrap_or(0),
            percentage_used: n.percentage_used,
            available_spare: n.available_spare,
            spare_threshold: n.available_spare_threshold,
            media_errors: n.media_errors,
            unsafe_shutdowns: n.unsafe_shutdowns,
        }),
        last_test,
        test_remaining: status.and_then(|s| s.remaining_percent),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEALTHY_ATA: &str = r#"{
      "smartctl":{"exit_status":0},
      "model_name":"Patriot P210 1024GB",
      "smart_status":{"passed":true},
      "temperature":{"current":34},
      "power_on_time":{"hours":4200},
      "ata_smart_attributes":{"table":[
        {"id":5,"name":"Reallocated_Sector_Ct","raw":{"value":0,"string":"0"}},
        {"id":9,"name":"Power_On_Hours","raw":{"value":4200,"string":"4200"}},
        {"id":197,"name":"Current_Pending_Sector","raw":{"value":0,"string":"0"}},
        {"id":198,"name":"Offline_Uncorrectable","raw":{"value":0,"string":"0"}}]},
      "ata_smart_data":{"self_test":{"status":{"value":0,"string":"completed without error","passed":true}}},
      "ata_smart_self_test_log":{"standard":{"table":[
        {"type":{"value":1,"string":"Short offline"},"status":{"value":0,"string":"Completed without error","passed":true},"lifetime_hours":4100}]}}
    }"#;

    const FAILING_ATA: &str = r#"{
      "smartctl":{"exit_status":8},
      "model_name":"Old Spinner",
      "smart_status":{"passed":false},
      "temperature":{"current":72},
      "ata_smart_attributes":{"table":[
        {"id":5,"raw":{"value":120}},{"id":197,"raw":{"value":8}},{"id":198,"raw":{"value":3}}]},
      "ata_smart_self_test_log":{"standard":{"table":[
        {"type":{"string":"Extended offline"},"status":{"string":"Completed: read failure","passed":false},"lifetime_hours":9000}]}}
    }"#;

    const NVME: &str = r#"{
      "smartctl":{"exit_status":0},
      "model_name":"Samsung SSD 980",
      "smart_status":{"passed":true},
      "nvme_smart_health_information_log":{
        "critical_warning":0,"temperature":41,"available_spare":100,"available_spare_threshold":10,
        "percentage_used":3,"media_errors":0,"power_on_hours":812,"unsafe_shutdowns":7}
    }"#;

    #[test]
    fn healthy_ata_drive() {
        let r = parse(HEALTHY_ATA).unwrap();
        assert_eq!(r.model, "Patriot P210 1024GB");
        assert_eq!((r.temp_c, r.power_on_hours), (Some(34), Some(4200)));
        assert_eq!(r.ata, Some(AtaStats { reallocated: Some(0), pending: Some(0), uncorrectable: Some(0) }));
        let t = r.last_test.as_ref().unwrap();
        assert_eq!((t.kind.as_str(), t.passed, t.hours), ("Short offline", true, Some(4100)));
        assert_eq!(r.verdict(), (Health::Good, vec![]));
    }

    #[test]
    fn failing_ata_drive_lists_every_reason() {
        let r = parse(FAILING_ATA).unwrap();
        let (h, why) = r.verdict();
        assert_eq!(h, Health::Failing);
        assert_eq!(why.len(), 6, "{why:?}");
        assert!(why[0].contains("failing"));
        assert!(why.iter().any(|w| w.contains("120 reallocated sectors")));
        assert!(why.iter().any(|w| w.contains("last self-test did not pass")));
        assert!(why.iter().any(|w| w.contains("72 °C")));
    }

    #[test]
    fn nvme_drive() {
        let r = parse(NVME).unwrap();
        assert!(r.ata.is_none());
        assert_eq!((r.temp_c, r.power_on_hours), (Some(41), Some(812)));
        let n = r.nvme.as_ref().unwrap();
        assert_eq!((n.percentage_used, n.available_spare, n.unsafe_shutdowns), (Some(3), Some(100), Some(7)));
        assert_eq!(r.verdict().0, Health::Good);
    }

    #[test]
    fn worn_nvme_is_a_warning_not_a_failure() {
        let worn = NVME
            .replace("\"percentage_used\":3", "\"percentage_used\":95")
            .replace("\"media_errors\":0", "\"media_errors\":2");
        let (h, why) = parse(&worn).unwrap().verdict();
        assert_eq!(h, Health::Warning);
        assert_eq!(why.len(), 2, "{why:?}");
        let critical = NVME.replace("\"critical_warning\":0", "\"critical_warning\":4");
        assert_eq!(parse(&critical).unwrap().verdict().0, Health::Failing);
    }

    #[test]
    fn running_self_test_is_reported() {
        let j = HEALTHY_ATA.replace(
            r#""status":{"value":0,"string":"completed without error","passed":true}}}"#,
            r#""status":{"value":249,"string":"in progress","remaining_percent":60}}}"#,
        );
        assert_eq!(parse(&j).unwrap().test_remaining, Some(60));
    }

    #[test]
    fn unreadable_drive_explains_why() {
        let j = r#"{"smartctl":{"exit_status":2,"messages":[
          {"string":"/dev/sdb: Permission denied","severity":"error"},{"string":"hint","severity":"information"}]}}"#;
        assert_eq!(parse(j).unwrap_err(), "/dev/sdb: Permission denied");
        let silent = r#"{"smartctl":{"exit_status":4}}"#;
        assert!(parse(silent).unwrap_err().contains("did not report"));
    }

    #[test]
    fn malformed_output_is_an_error_not_a_panic() {
        for j in ["", "not json", "[]", "{\"smart_status\":5}", "{\"smartctl\":null"] {
            assert!(parse(j).is_err(), "{j:?}");
        }
    }

    #[test]
    fn a_drive_that_only_says_passed_is_good() {
        let r = parse(r#"{"smart_status":{"passed":true}}"#).unwrap();
        assert_eq!(r.verdict().0, Health::Good);
        let r = parse(r#"{"smart_status":{},"temperature":{"current":30}}"#).unwrap();
        assert_eq!(r.verdict().0, Health::Unknown);
    }
}
