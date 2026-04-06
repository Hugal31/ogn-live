use std::collections::HashMap;

use aprs::PositionReport;
use ogn::ddb::Device;

const KNOTS_TO_KMH: f64 = 1.852;
const FPM_TO_MPS: f64 = 0.00508;

pub struct ReportFormatter {
    pub receiver: String,
    pub database: HashMap<u32, Device>,
}

impl ReportFormatter {
    pub async fn new(receiver: String) -> Self {
        Self {
            receiver,
            database: Self::get_ogn_ddb().await,
        }
    }

    async fn get_ogn_ddb() -> HashMap<u32, Device> {
        use ogn::ddb::{OGN_DDB_URL, index_by_id, read_database};
        let body = reqwest::get(OGN_DDB_URL)
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        read_database(body.as_bytes()).map(index_by_id).unwrap()
    }

    /// Output in this format:
    /// ```
    /// <m a="46.1,5.1,OI,G-EZOI,2371,11:16:45,20,135,482,0.0,0,Grenoble,0,d4de7516"/>
    /// ```
    /// Lat,Lon,Imat Short, Imat, Alt(m), Last heard time (which TZ?),ddf,Track,Ground speed (km/h),Vz (m/s),Type,Receiver,shownId,id
    /// ddf = secs since last heard?
    ///
    pub fn format(&self, report: &PositionReport) -> String {
        log::debug!("Formatting {report:?}");
        let position = report.point();
        let id = report.id();
        let (device, beacon) = if let Some(ref id) = id
            && let Ok(beacon) = id.parse::<ogn::Beacon>()
        {
            (self.database.get(&beacon.address), Some(beacon))
        } else {
            (None, None)
        };
        if let Some(device) = device {
            log::debug!("Device is {device:?}");
        }
        let id = id.as_deref().unwrap_or("").to_ascii_lowercase();
        let immat = device.map(|d| &d.registration).unwrap_or(&id);
        let short_immat = if let Some(device) = device {
            device.common_name.clone()
        } else if immat.len() < 2 {
            "??".to_string()
        } else {
            format!(
                "_{}{}",
                id.chars().nth(id.len() - 2).unwrap(),
                id.chars().nth(id.len() - 1).unwrap()
            )
        };
        let alt_m = report.altitude().unwrap_or(0.) * 0.3048;
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<markers>
<m a="{lat:.5},{lon:.5},{short_immat},{immat},{alt},{time},0,{track},{gs},{vz},{typ},{recv},{shown_id},{id}"/>
</markers>"#,
            lat = position.y(),
            lon = position.x(),
            short_immat = short_immat,
            immat = immat,
            alt = alt_m,
            time = report
                .timestamp
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            track = report.course().unwrap_or(0),
            gs = report.speed().unwrap_or(0.) as f64 * KNOTS_TO_KMH,
            vz = report.climb_rate().unwrap_or(0.) * FPM_TO_MPS,
            typ = beacon.as_ref().map(|b| b.aircraft_type.into()).unwrap_or(0),
            recv = self.receiver,
            shown_id = beacon
                .map(|b| if b.no_tracking {
                    "0".to_owned()
                } else {
                    format!("{:X}", b.address)
                })
                .unwrap_or(id.clone()),
            id = id,
        )
    }
}
