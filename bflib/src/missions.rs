use anyhow::{bail, Result};
use dcso3::Vector3;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ObjectiveType {
    CombatAirPatrol,
    DeliverCrate,
    TransportTroops,
    DestroyVehicles,
    DestroyAircraft,
    ProtectVehicles,
    ProtectAircraft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionID {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mission {
    name: String,
    objectives: Vec<Objective>,
    pmc: String,
    reward: isize,
    expiry: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Objective {
    name: String,
    objective_type: ObjectiveType,
    units_to_kill: Vec<String>,
    units_to_protect: Vec<String>,
    target_points_radius: Vec<(Vector3, isize)>,
    cargo_to_deliver: Option<Vec<String>>,
}

impl Mission {
    pub fn new() -> Result<Self> {
        todo!()
    }

    pub fn check_expire(self, time: f64) -> Option<f64> {
        if time > self.expiry {
            return None;
        }
        Some(self.expiry - time)
    }

    pub fn check_next_objective(self) -> Result<bool> {
        match self.objectives.get(self.objectives.len() - 1) {
            Some(o) => o.check_status()?,
            None => bail!("no objectives left"),
        };
        Ok(false)
    }
}

impl Objective {
    pub fn check_status(&self) -> Result<bool> {

        match self.objective_type {
            ObjectiveType::CombatAirPatrol => todo!(),
            ObjectiveType::DeliverCrate => todo!(),
            ObjectiveType::TransportTroops => todo!(),
            ObjectiveType::DestroyVehicles => todo!(),
            ObjectiveType::DestroyAircraft => todo!(),
            ObjectiveType::ProtectVehicles => todo!(),
            ObjectiveType::ProtectAircraft => todo!(),
        };

        Ok(false)
    }
}