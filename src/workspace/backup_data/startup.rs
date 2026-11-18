//! Additive migration of portable Java window startup choices.

use std::io;
use std::ops::RangeInclusive;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::profile::WindowPlacement;
use crate::workspace::window_lifecycle::FixedGeometry;

use super::{
    RawFields, java_flag, merge_unset_section, preference, preference_node, preference_text,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    remember_geometry: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remember_dashboard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    use_fixed_geometry: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fixed: Option<FixedGeometry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    java_last_geometry: Option<FixedGeometry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    java_dashboard_visible: Option<bool>,
}

fn dimension<T: Copy + Into<f64>>(
    geometry: Node<'_, '_>,
    name: &str,
    limits: RangeInclusive<T>,
) -> Result<f64, ()> {
    let value = preference_text(geometry, name)?
        .ok_or(())?
        .parse::<f64>()
        .map_err(|_| ())?;
    ((*limits.start()).into()..=(*limits.end()).into())
        .contains(&value)
        .then(|| value.round())
        .ok_or(())
}

fn geometry(root: Node<'_, '_>, name: &str) -> Result<Option<FixedGeometry>, ()> {
    let Some(geometry) = preference_node(root, name)? else {
        return Ok(None);
    };
    // Java reapplies the saved bounds and maximization on launch. Its geometry
    // record can carry a fullscreen field, but startup does not use it.
    let maximized = match preference_text(geometry, "maximized")? {
        Some(value) => java_flag(value).ok_or(())?,
        None => false,
    };
    let (width, height) = if name == "fixedWindowGeometry" {
        (FixedGeometry::WIDTH, FixedGeometry::HEIGHT)
    } else {
        (WindowPlacement::WIDTH, WindowPlacement::HEIGHT)
    };
    Ok(Some(FixedGeometry {
        width: dimension(geometry, "width", width)? as u16,
        height: dimension(geometry, "height", height)? as u16,
        x: dimension(geometry, "x", FixedGeometry::POSITION)? as i32,
        y: dimension(geometry, "y", FixedGeometry::POSITION)? as i32,
        maximized,
    }))
}

fn read_geometry(root: Node<'_, '_>, name: &str, omitted: &mut usize) -> Option<FixedGeometry> {
    match geometry(root, name) {
        Ok(bounds) => bounds,
        Err(()) => {
            *omitted += 1;
            None
        }
    }
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let fixed = read_geometry(root, "fixedWindowGeometry", omitted);
        let use_fixed_geometry = preference(root, "useFixedWindowGeometry", omitted, |text| {
            java_flag(text).filter(|&enabled| !enabled || fixed.is_some())
        });
        let settings = Self {
            remember_geometry: preference(root, "rememberWindowGeometry", omitted, java_flag),
            remember_dashboard: preference(root, "rememberDashboardState", omitted, java_flag),
            use_fixed_geometry,
            fixed,
            java_last_geometry: read_geometry(root, "lastWindowGeometry", omitted),
            java_dashboard_visible: preference(root, "dashboardVisible", omitted, java_flag),
        };
        (settings.count() != 0).then_some(settings)
    }

    pub(super) fn count(&self) -> usize {
        [
            self.remember_geometry.is_some(),
            self.remember_dashboard.is_some(),
            self.use_fixed_geometry.is_some(),
            self.java_dashboard_visible.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
            + if self.fixed.is_some() { 5 } else { 0 }
            + if self.java_last_geometry.is_some() {
                5
            } else {
                0
            }
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self.fixed.is_some_and(|fixed| fixed.clamped() != fixed)
            || self.use_fixed_geometry == Some(true) && self.fixed.is_none()
            || self
                .java_last_geometry
                .is_some_and(|bounds| !bounds.remembered_valid())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java fixed window geometry",
            ));
        }
        let mut startup: RawFields = fields
            .get("startup")
            .map(|value| serde_json::from_str(value.get()))
            .transpose()?
            .unwrap_or_default();
        let mut incoming: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        incoming.remove("fixed");
        let before = startup.len();
        for (name, value) in incoming {
            startup.entry(name).or_insert(value);
        }
        let fixed_added = self
            .fixed
            .as_ref()
            .map(|fixed| merge_unset_section(&mut startup, "fixed", fixed))
            .transpose()?
            .unwrap_or(false);
        if fixed_added || startup.len() != before {
            fields.insert("startup".into(), serde_json::value::to_raw_value(&startup)?);
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{merge_java_window_settings, portable_java_preferences};

    #[test]
    fn java_startup_preferences_reject_fixed_geometry_without_native_bounds() {
        for shape in [
            "<fixedWindowGeometry><x>-100001</x><y>80</y><width>1200</width><height>800</height></fixedWindowGeometry>",
            "<fixedWindowGeometry><x>100</x><y>80</y><width>NaN</width><height>800</height></fixedWindowGeometry>",
            "<fixedWindowGeometry><x>100</x><y>80</y><width>1200</width></fixedWindowGeometry>",
        ] {
            let xml = format!(
                "<globalSettings><useFixedWindowGeometry>true</useFixedWindowGeometry>
                <rememberDashboardState>false</rememberDashboardState>{shape}</globalSettings>"
            );
            let incoming = portable_java_preferences(xml.as_bytes(), None).unwrap();
            assert_eq!((incoming.count, incoming.omitted), (1, 2));
            let actual =
                merge_java_window_settings(None, &incoming.contents.unwrap(), &Default::default())
                    .unwrap()
                    .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&actual).unwrap(),
                serde_json::json!({"startup":{"remember_dashboard":false}})
            );
        }
    }
}
