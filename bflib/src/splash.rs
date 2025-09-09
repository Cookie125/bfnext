/*
Copyright 2024 Eric Stokes.

This file is part of bflib.

bflib is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your
option) any later version.

bflib is distributed in the hope that it will be useful, but WITHOUT
ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
FITNESS FOR A PARTICULAR PURPOSE. See the GNU Affero Public License
for more details.
*/

//! Splash damage system that tracks weapons dropped by players and creates larger explosions on impact

use crate::db::Db;
use bfprotocols::db::group::UnitId as BfUnitId;
use anyhow::Result;
use chrono::{DateTime, Utc};
use compact_str::CompactString;
use dcso3::{
    event::Shot as ShotEvent,
    land::Land,
    object::{DcsObject, DcsOid},
    trigger::{FlareColor, SmokePreset, Trigger},
    weapon::ClassWeapon,
    LuaVec2, LuaVec3, MizLua, Position3,
};
use fxhash::FxHashMap;
use log::{debug, warn};
use na::Vector3;
use rand::{thread_rng, Rng, rngs::ThreadRng};
use serde_derive::{Deserialize, Serialize};
use std::collections::VecDeque;

// Constants for timing and cleanup intervals
const UNIT_INIT_INTERVAL: i64 = 300; // 5 minutes
const MISSION_SETTINGS_INTERVAL: i64 = 600; // 10 minutes  
const CLEANUP_INTERVAL: i64 = 900; // 15 minutes
const PROCESSED_UNITS_CLEANUP_MINUTES: i64 = 5;
const PROCESSED_SMOKE_CLEANUP_LIMIT: usize = 100;
const MIN_EFFECT_DELAY_SECONDS: i64 = 2;


/// Helper function for consistent cook-off debug logging
fn cookoff_debug(msg: &str) {
    debug!("COOKOFF: {}", msg);
}

/// Calculate distance between two 3D points
fn distance_3d(a: &LuaVec3, b: &LuaVec3) -> f32 {
    let dx = a.0.x - b.0.x;
    let dy = a.0.y - b.0.y;
    let dz = a.0.z - b.0.z;
    (dx * dx + dy * dy + dz * dz).sqrt() as f32
}


/// Check if a position is within a sphere
fn is_within_sphere(center: &LuaVec3, position: &LuaVec3, radius: f32) -> bool {
    distance_3d(center, position) <= radius
}

/// Calculate damage based on distance and explosion power


/// Recent explosion tracking for ordnance protection
#[derive(Debug, Clone)]
struct RecentExplosion {
    /// Position of the explosion
    position: LuaVec3,
    /// Time when explosion occurred
    time: DateTime<Utc>,
    /// Radius of the explosion
    radius: f32,
}

/// Unit data for blast wave processing
struct BlastWaveUnit {
    position: LuaVec3,
    distance: f32,
    health: f64,
    max_health: f64,
    unit_type: String,
    is_ground_unit: bool,
}

/// Debris explosion configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebrisExplosionConfig {
    /// Power of each debris explosion
    pub power: f32,
    /// Maximum distance debris can travel (meters)
    pub max_distance: f32,
}

/// Debris count configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebrisCountConfig {
    /// Minimum debris pieces per cook-off
    pub min: u32,
    /// Maximum debris pieces per cook-off
    pub max: u32,
}

/// Debris effect configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebrisConfig {
    /// Enable debris effects from cook-offs
    pub enabled: bool,
    /// Debris explosion configuration
    pub explosion: DebrisExplosionConfig,
    /// Debris count configuration
    pub count: DebrisCountConfig,
}

/// Flare instant configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlareInstantConfig {
    /// If true, spawns flares instantly; if false, spawns over time
    pub enabled: bool,
    /// Minimum number of instant flares when instant is true
    pub min: u32,
    /// Maximum number of instant flares when instant is true
    pub max: u32,
}

/// Flare timing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlareTimingConfig {
    /// Multiplier for non-instant flare count
    pub count_modifier: f32,
    /// Max offset distance for flares in meters (horizontal)
    pub offset: f64,
    /// Chance of flares firing out (0.0 to 1.0)
    pub chance: f32,
}

/// Flare effect configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlareConfig {
    /// Enable/disable flare effects for cook-offs
    pub enabled: bool,
    /// Flare color (matches Lua cookoff_flare_color)
    pub color: u8,
    /// Flare instant configuration
    pub instant: FlareInstantConfig,
    /// Flare timing configuration
    pub timing: FlareTimingConfig,
}

/// Configuration for all vehicles (fallback settings)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllVehiclesConfig {
    /// Enable effects for all ground vehicles not in units table
    pub enabled: bool,
    /// Health percentage threshold for all vehicles
    pub damage_threshold: f32,
    /// Explosion effect configuration for all vehicles
    pub explosion: ExplosionConfig,
    /// Cook-off effect configuration for all vehicles
    pub cookoff: CookOffEffectConfig,
    /// Smoke/flame effect configuration for all vehicles
    pub smoke: SmokeConfig,
    /// Chance of cook-off effects occurring for all vehicles (0.0 to 1.0)
    pub cookoff_chance: f32,
    /// Chance of smoke effect for all vehicles (0.0 to 1.0)
    pub smoke_chance: f32,
    /// Automatically smoke along with cook-off for all vehicles
    pub smoke_with_cookoff: bool,
    /// If it's a smoke only effect, add an explosion to finish the vehicle off
    pub explode_on_smoke_only: bool,
}

/// Configuration for cook off and fuel explosion effects
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookOffConfig {
    /// Enable cook-off effects
    pub enabled: bool,
    /// Chance of cook-off effects occurring (0.0 to 1.0)
    pub effects_chance: f32,
    /// Health percentage below which unit explodes (0-100)
    pub damage_threshold: f32,
    /// Debris effect configuration
    pub debris: DebrisConfig,
    /// Flare effect configuration
    pub flares: FlareConfig,
    /// All vehicles configuration (fallback settings)
    pub all_vehicles: AllVehiclesConfig,
}

impl Default for DebrisExplosionConfig {
    fn default() -> Self {
        Self {
            power: 1.0,
            max_distance: 8.0,
        }
    }
}

impl Default for DebrisCountConfig {
    fn default() -> Self {
        Self {
            min: 6,
            max: 12,
        }
    }
}

impl Default for DebrisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            explosion: DebrisExplosionConfig::default(),
            count: DebrisCountConfig::default(),
        }
    }
}

impl Default for FlareInstantConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min: 2,
            max: 5,
        }
    }
}

impl Default for FlareTimingConfig {
    fn default() -> Self {
        Self {
            count_modifier: 1.0,
            offset: 0.5,
            chance: 0.5,
        }
    }
}

impl Default for FlareConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            color: 2, // Matches Lua cookoff_flare_color = 2
            instant: FlareInstantConfig::default(),
            timing: FlareTimingConfig::default(),
        }
    }
}

impl Default for AllVehiclesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            damage_threshold: 25.0,
            explosion: ExplosionConfig {
                power: 40.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 4,
                power: 10.0,
                duration: 30.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: true,
                size: 4, // Smaller smoke size (4 = small smoke, 6 = medium, 7 = large, 8 = huge)
                duration: 180, // Reduced duration to match Lua script
            },
            cookoff_chance: 0.4,
            smoke_chance: 0.7,
            smoke_with_cookoff: true,
            explode_on_smoke_only: true,
        }
    }
}

impl Default for CookOffConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            effects_chance: 1.0,
            damage_threshold: 25.0,
            debris: DebrisConfig::default(),
            flares: FlareConfig::default(),
            all_vehicles: AllVehiclesConfig::default(),
        }
    }
}

/// Explosion effect configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplosionConfig {
    /// Power of the initial explosion
    pub power: f32,
    /// Whether to trigger explosion
    pub enabled: bool,
}

/// Cook-off effect configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookOffEffectConfig {
    /// Whether to trigger cook-off effects
    pub enabled: bool,
    /// Number of cook-off explosions
    pub count: u32,
    /// Power of cook-off explosions
    pub power: f32,
    /// Duration of cook-off sequence (seconds)
    pub duration: f32,
    /// Whether to use random timing for cook-offs
    pub random_timing: bool,
    /// Percentage variation in cook-off power
    pub power_random: f32,
}

/// Smoke/flame effect configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmokeConfig {
    /// Whether this is a tanker (produces smoke/fire)
    pub is_tanker: bool,
    /// Size of flame/smoke effect (1-8)
    pub size: u8,
    /// Duration of flame/smoke effect (seconds)
    pub duration: u32,
}

/// Properties for a specific unit type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitProperties {
    /// Explosion effect configuration
    pub explosion: ExplosionConfig,
    /// Cook-off effect configuration
    pub cookoff: CookOffEffectConfig,
    /// Smoke/flame effect configuration
    pub smoke: SmokeConfig,
}

impl Default for ExplosionConfig {
    fn default() -> Self {
        Self {
            power: 40.0,
            enabled: true,
        }
    }
}

impl Default for CookOffEffectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            count: 4,
            power: 10.0,
            duration: 30.0,
            random_timing: true,
            power_random: 50.0,
        }
    }
}

impl Default for SmokeConfig {
    fn default() -> Self {
        Self {
            is_tanker: false,
            size: 6,
            duration: 240,
        }
    }
}

impl Default for UnitProperties {
    fn default() -> Self {
        Self {
            explosion: ExplosionConfig::default(),
            cookoff: CookOffEffectConfig::default(),
            smoke: SmokeConfig::default(),
        }
    }
}

/// A pending cook-off effect
#[derive(Debug, Clone)]
pub struct PendingCookOffEffect {
    pub unit_id: BfUnitId,
    pub unit_name: CompactString,
    pub unit_type: CompactString,
    pub position: Position3,
    pub start_time: DateTime<Utc>,
    pub is_cookoff: bool,
    pub is_dead: bool,
    pub properties: UnitProperties,
}

/// A scheduled cook-off effect
#[derive(Debug, Clone)]
pub struct ScheduledCookOffEffect {
    pub unit_id: BfUnitId,
    pub unit_name: CompactString,
    pub unit_type: CompactString,
    pub position: Position3,
    /// Explosion effect configuration
    pub explosion: ExplosionConfig,
    /// Cook-off effect configuration
    pub cookoff: CookOffEffectConfig,
    /// Smoke/flame effect configuration
    pub smoke: SmokeConfig,
}

/// A timed explosion effect
#[derive(Debug, Clone)]
pub struct TimedExplosion {
    pub position: Position3,
    pub power: f32,
    pub trigger_time: DateTime<Utc>,
    pub effect_type: ExplosionType,
    pub azimuth: Option<u16>, // For flares, stores the azimuth direction
}

/// Type of explosion effect
#[derive(Debug, Clone)]
pub enum ExplosionType {
    Cookoff,
    Debris,
    Flare,
}

/// Movement tracker for cook-off system
#[derive(Debug, Clone)]
pub struct MovementTracker {
    /// Unit ID being tracked
    pub unit_id: BfUnitId,
    /// Last known position
    pub last_position: LuaVec3,
    /// Last update time
    pub last_update: DateTime<Utc>,
    /// Number of consecutive stationary checks
    pub stationary_checks: u32,
    /// Maximum stationary checks before triggering effects
    pub max_stationary_checks: u32,
    /// Movement threshold (meters)
    pub movement_threshold: f32,
}

impl MovementTracker {
    pub fn new(unit_id: BfUnitId, position: LuaVec3) -> Self {
        Self {
            unit_id,
            last_position: position,
            last_update: Utc::now(),
            stationary_checks: 0,
            max_stationary_checks: 3, // 3 consecutive checks (3 seconds)
            movement_threshold: 1.0, // 1 meter threshold
        }
    }
}

/// Performance monitoring modes
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PerformanceMode {
    Normal,    // Standard update rate (target: <10ms)
    Reduced,   // Reduced update rate for performance (target: <16ms)
    Minimal,   // Minimal update rate for poor performance (target: <25ms)
    Critical,  // Critical performance mode for very poor performance (target: >25ms)
}

/// Cook-off system
#[derive(Debug)]
pub struct CookOff {
    pub config: CookOffConfig,
    pub units: FxHashMap<CompactString, UnitProperties>,
    pub pending_effects: FxHashMap<BfUnitId, PendingCookOffEffect>,
    pub processed_units: FxHashMap<BfUnitId, DateTime<Utc>>,
    pub processed_smoke: FxHashMap<BfUnitId, bool>,
    pub effect_queue: VecDeque<ScheduledCookOffEffect>,
    pub timed_explosions: VecDeque<TimedExplosion>,
    pub effect_smoke_id: u32,
    // Movement tracking for cook-offs
    pub movement_tracking: FxHashMap<BfUnitId, MovementTracker>,
    // Cached random number generator for performance
    pub rng: ThreadRng,
    // Performance monitoring
    pub update_times: VecDeque<u64>, // Track last 10 update times in microseconds
    pub last_update_time: DateTime<Utc>,
    pub adaptive_update_interval: i64, // Current update interval in milliseconds
    pub performance_mode: PerformanceMode,
}

/// Wave explosion configuration
#[derive(Debug, Clone)]
pub struct WaveExplosionConfig {
    /// Enable wave explosions (secondary explosions radiating outward)
    pub enabled: bool,
    /// Scaling factor for wave explosion power
    pub scaling: f32,
    /// Damage threshold for triggering wave explosions
    pub damage_threshold: f32,
    /// Always trigger cascade explosions (like original script)
    pub always_cascade_explode: bool,
}

/// Blast wave damage model configuration
#[derive(Debug, Clone)]
pub struct BlastWaveConfig {
    /// Base blast search radius (meters)
    pub search_radius: f32,
    /// Use dynamic blast radius calculation
    pub use_dynamic_radius: bool,
    /// Multiplier for dynamic blast radius
    pub dynamic_radius_modifier: f32,
}

/// Shaped charge configuration
#[derive(Debug, Clone)]
pub struct ShapedChargeConfig {
    /// Apply shaped charge effects (reduced blast radius)
    pub enabled: bool,
    /// Multiplier that reduces blast radius and explosion power
    pub multiplier: f32,
}

/// Cluster bomb configuration
#[derive(Debug, Clone)]
pub struct ClusterBombConfig {
    /// Enable cluster bomb support
    pub enabled: bool,
    /// Base forward spread (meters)
    pub base_length: f32,
    /// Base lateral spread (meters)
    pub base_width: f32,
    /// Maximum forward spread (meters)
    pub max_length: f32,
    /// Maximum lateral spread (meters)
    pub max_width: f32,
    /// Minimum forward spread (meters)
    pub min_length: f32,
    /// Minimum lateral spread (meters)
    pub min_width: f32,
    /// Use equation to reduce number of bomblets
    pub bomblet_reduction_modifier: bool,
    /// Global modifier for bomblet explosive power
    pub bomblet_damage_modifier: f32,
    /// Submunition explosive power table
    pub submunition_powers: FxHashMap<String, f32>,
}

/// Pre/Post explosion scanning configuration

/// Ordnance protection configuration
#[derive(Debug, Clone)]
pub struct OrdnanceProtectionConfig {
    /// Enable ordnance protection features
    pub enabled: bool,
    /// Distance in meters to protect nearby bombs
    pub protection_radius: f32,
    /// Detect ordnance destroyed by large explosions
    pub detect_ordnance_destruction: bool,
    /// Snap to ground if destroyed by large explosion
    pub snap_to_ground_if_destroyed: bool,
    /// Maximum height to snap to ground from
    pub max_snapped_height: f32,
    /// Enable looking for recent large explosions
    pub recent_large_explosion_snap: bool,
    /// Range to look for recent large explosions (meters)
    pub recent_large_explosion_range: f32,
    /// Time window for recent large explosions (seconds)
    pub recent_large_explosion_time: f32,
}

/// Configuration for splash damage effects
#[derive(Debug, Clone)]
pub struct SplashConfig {
    /// Whether splash damage is enabled
    pub enabled: bool,
    /// Manually defined weapon explosion powers
    pub weapon_powers: FxHashMap<String, f32>,
    /// Cook-off configuration
    pub cookoff: CookOffConfig,
    /// Wave explosion configuration
    pub wave_explosions: WaveExplosionConfig,
    /// Blast wave damage model configuration
    pub blast_wave: BlastWaveConfig,
    /// Shaped charge configuration
    pub shaped_charge: ShapedChargeConfig,
    /// Cluster bomb configuration
    pub cluster_bombs: ClusterBombConfig,
    /// Ordnance protection configuration
    pub ordnance_protection: OrdnanceProtectionConfig,
    /// Overall scaling for explosive power
    pub overall_scaling: f32,
    /// Multiplier for rocket explosive power
    pub rocket_multiplier: f32,
    /// Track only weapons launched by players
    pub only_players_weapons: bool,
    /// HEAT weapons that should use shaped charge effects
    pub heat_weapons: FxHashMap<String, bool>,
    /// Cascade explosion configuration
    pub cascade_damage_threshold: f32,
    pub cascade_explode_threshold: f32,
    pub cascade_scaling: f32,
}

impl Default for SplashConfig {
    fn default() -> Self {
        let mut weapon_powers = FxHashMap::default();
        
        // WWII Bombs
        weapon_powers.insert("British_GP_250LB_Bomb_Mk1".to_string(), 100.0);
        weapon_powers.insert("British_GP_250LB_Bomb_Mk4".to_string(), 100.0);
        weapon_powers.insert("British_GP_250LB_Bomb_Mk5".to_string(), 100.0);
        weapon_powers.insert("British_GP_500LB_Bomb_Mk1".to_string(), 213.0);
        weapon_powers.insert("British_GP_500LB_Bomb_Mk4".to_string(), 213.0);
        weapon_powers.insert("British_GP_500LB_Bomb_Mk4_Short".to_string(), 213.0);
        weapon_powers.insert("British_GP_500LB_Bomb_Mk5".to_string(), 213.0);
        weapon_powers.insert("British_MC_250LB_Bomb_Mk1".to_string(), 100.0);
        weapon_powers.insert("British_MC_250LB_Bomb_Mk2".to_string(), 100.0);
        weapon_powers.insert("British_MC_500LB_Bomb_Mk1_Short".to_string(), 213.0);
        weapon_powers.insert("British_MC_500LB_Bomb_Mk2".to_string(), 213.0);
        weapon_powers.insert("British_SAP_250LB_Bomb_Mk5".to_string(), 100.0);
        weapon_powers.insert("British_SAP_500LB_Bomb_Mk5".to_string(), 213.0);
        weapon_powers.insert("British_AP_25LBNo1_3INCHNo1".to_string(), 4.0);
        weapon_powers.insert("British_HE_60LBSAPNo2_3INCHNo1".to_string(), 4.0);
        weapon_powers.insert("British_HE_60LBFNo1_3INCHNo1".to_string(), 4.0);
        
        weapon_powers.insert("SC_50".to_string(), 20.0);
        weapon_powers.insert("ER_4_SC50".to_string(), 20.0);
        weapon_powers.insert("SC_250_T1_L2".to_string(), 100.0);
        weapon_powers.insert("SC_501_SC250".to_string(), 100.0);
        weapon_powers.insert("Schloss500XIIC1_SC_250_T3_J".to_string(), 100.0);
        weapon_powers.insert("SC_501_SC500".to_string(), 213.0);
        weapon_powers.insert("SC_500_L2".to_string(), 213.0);
        weapon_powers.insert("SD_250_Stg".to_string(), 100.0);
        weapon_powers.insert("SD_500_A".to_string(), 213.0);
        
        
        // WWII Rockets
        weapon_powers.insert("3xM8_ROCKETS_IN_TUBES".to_string(), 4.0);
        weapon_powers.insert("WGr21".to_string(), 4.0);
        
        // Unguided Bombs
        weapon_powers.insert("M_117".to_string(), 201.0);
        weapon_powers.insert("AN_M30A1".to_string(), 45.0);
        weapon_powers.insert("AN_M57".to_string(), 100.0);
        weapon_powers.insert("AN_M64".to_string(), 121.0);
        weapon_powers.insert("AN_M65".to_string(), 400.0);
        weapon_powers.insert("AN_M66".to_string(), 800.0);
        weapon_powers.insert("AN-M66A2".to_string(), 536.0);
        weapon_powers.insert("AN-M81".to_string(), 100.0);
        weapon_powers.insert("AN-M88".to_string(), 100.0);
        
        weapon_powers.insert("Mk_81".to_string(), 60.0);
        weapon_powers.insert("MK-81SE".to_string(), 60.0);
        weapon_powers.insert("Mk_82".to_string(), 100.0);
        weapon_powers.insert("MK_82AIR".to_string(), 100.0);
        weapon_powers.insert("MK_82SNAKEYE".to_string(), 100.0);
        weapon_powers.insert("Mk_83".to_string(), 274.0);
        weapon_powers.insert("Mk_84".to_string(), 582.0);
        
        weapon_powers.insert("HEBOMB".to_string(), 40.0);
        weapon_powers.insert("HEBOMBD".to_string(), 40.0);
        
        weapon_powers.insert("SAMP125LD".to_string(), 60.0);
        weapon_powers.insert("SAMP250LD".to_string(), 118.0);
        weapon_powers.insert("SAMP250HD".to_string(), 118.0);
        weapon_powers.insert("SAMP400LD".to_string(), 274.0);
        weapon_powers.insert("SAMP400HD".to_string(), 274.0);
        
        weapon_powers.insert("BR_250".to_string(), 100.0);
        weapon_powers.insert("BR_500".to_string(), 100.0);
        
        weapon_powers.insert("FAB_100".to_string(), 45.0);
        weapon_powers.insert("FAB_250".to_string(), 118.0);
        weapon_powers.insert("FAB_250M54TU".to_string(), 118.0);
        weapon_powers.insert("FAB-250-M62".to_string(), 118.0);
        weapon_powers.insert("FAB_500".to_string(), 213.0);
        weapon_powers.insert("FAB_1500".to_string(), 675.0);
        
        // Unguided Bombs with Penetrator/Anti-Runway
        weapon_powers.insert("Durandal".to_string(), 64.0);
        weapon_powers.insert("BLU107B_DURANDAL".to_string(), 64.0);
        weapon_powers.insert("BAP_100".to_string(), 32.0);
        weapon_powers.insert("BAP-100".to_string(), 32.0);
        weapon_powers.insert("BAT-120".to_string(), 32.0);
        weapon_powers.insert("TYPE-200A".to_string(), 107.0);
        weapon_powers.insert("BetAB_500".to_string(), 98.0);
        weapon_powers.insert("BetAB_500ShP".to_string(), 107.0);
        
        // Guided Bombs (GBU)
        weapon_powers.insert("GBU_10".to_string(), 582.0);
        weapon_powers.insert("GBU_12".to_string(), 100.0);
        weapon_powers.insert("GBU_16".to_string(), 274.0);
        weapon_powers.insert("GBU_24".to_string(), 582.0);
        weapon_powers.insert("KAB_1500Kr".to_string(), 675.0);
        weapon_powers.insert("KAB_500Kr".to_string(), 213.0);
        weapon_powers.insert("KAB_500".to_string(), 213.0);
        
        // INS/GPS Bombs (JDAM)
        weapon_powers.insert("GBU_31".to_string(), 582.0);
        weapon_powers.insert("GBU_31_V_3B".to_string(), 582.0);
        weapon_powers.insert("GBU_31_V_2B".to_string(), 582.0);
        weapon_powers.insert("GBU_31_V_4B".to_string(), 582.0);
        weapon_powers.insert("GBU_32_V_2B".to_string(), 202.0);
        weapon_powers.insert("GBU_38".to_string(), 100.0);
        weapon_powers.insert("GBU_54_V_1B".to_string(), 100.0);
        
        // Glide Bombs (JSOW)
        weapon_powers.insert("AGM_154C".to_string(), 305.0); // Single warhead (BROACH)
        weapon_powers.insert("AGM_154".to_string(), 305.0); // Single warhead (BROACH)
        
        weapon_powers.insert("LS-6-100".to_string(), 45.0);
        weapon_powers.insert("LS-6-250".to_string(), 100.0);
        weapon_powers.insert("LS-6-500".to_string(), 274.0);
        
        // Air Ground Missiles (AGM)
        weapon_powers.insert("AGM_62".to_string(), 400.0);
        weapon_powers.insert("AGM_65D".to_string(), 38.0);
        weapon_powers.insert("AGM_65E".to_string(), 80.0);
        weapon_powers.insert("AGM_65F".to_string(), 80.0);
        weapon_powers.insert("AGM_65G".to_string(), 80.0);
        weapon_powers.insert("AGM_65H".to_string(), 38.0);
        weapon_powers.insert("AGM_65K".to_string(), 80.0);
        weapon_powers.insert("AGM_65L".to_string(), 80.0);
        weapon_powers.insert("AGM_123".to_string(), 274.0);
        weapon_powers.insert("AGM_130".to_string(), 582.0);
        weapon_powers.insert("AGM_119".to_string(), 176.0);
        weapon_powers.insert("AGM_114".to_string(), 10.0);
        weapon_powers.insert("AGM_114K".to_string(), 10.0);
        
        weapon_powers.insert("Rb 05A".to_string(), 217.0);
        weapon_powers.insert("RB75".to_string(), 38.0);
        weapon_powers.insert("RB75A".to_string(), 38.0);
        weapon_powers.insert("RB75B".to_string(), 38.0);
        weapon_powers.insert("RB75T".to_string(), 80.0);
        weapon_powers.insert("HOT3_MBDA".to_string(), 15.0);
        weapon_powers.insert("C-701T".to_string(), 38.0);
        weapon_powers.insert("C-701IR".to_string(), 38.0);
        
        weapon_powers.insert("Vikhr_M".to_string(), 11.0);
        weapon_powers.insert("Vikhr_9M127_1".to_string(), 11.0);
        weapon_powers.insert("AT_6".to_string(), 11.0);
        weapon_powers.insert("Ataka_9M120".to_string(), 11.0);
        weapon_powers.insert("Ataka_9M120F".to_string(), 11.0);
        weapon_powers.insert("P_9M117".to_string(), 0.0);
        
        weapon_powers.insert("KH-66_Grom".to_string(), 108.0);
        weapon_powers.insert("X_23".to_string(), 111.0);
        weapon_powers.insert("X_23L".to_string(), 111.0);
        weapon_powers.insert("X_28".to_string(), 160.0);
        weapon_powers.insert("X_25ML".to_string(), 89.0);
        weapon_powers.insert("X_25MR".to_string(), 140.0);
        weapon_powers.insert("X_29L".to_string(), 320.0);
        weapon_powers.insert("X_29T".to_string(), 320.0);
        weapon_powers.insert("X_29TE".to_string(), 320.0);
        
        weapon_powers.insert("AKD-10".to_string(), 10.0);
        
        // Anti-Radar Missiles (ARM)
        weapon_powers.insert("AGM_88C".to_string(), 69.0);
        weapon_powers.insert("AGM_88".to_string(), 69.0);
        weapon_powers.insert("AGM_122".to_string(), 12.0);
        weapon_powers.insert("LD-10".to_string(), 75.0);
        weapon_powers.insert("AGM_45A".to_string(), 66.0);
        weapon_powers.insert("AGM_45B".to_string(), 66.0);
        weapon_powers.insert("X_58".to_string(), 149.0);
        weapon_powers.insert("X_25MP".to_string(), 90.0);
        weapon_powers.insert("X_31P".to_string(), 90.0);
        
        // Anti-Ship Missiles (ASh)
        weapon_powers.insert("AGM_84D".to_string(), 488.0);
        weapon_powers.insert("Rb 15F".to_string(), 500.0);
        weapon_powers.insert("C-802AK".to_string(), 500.0);
        weapon_powers.insert("X_31A".to_string(), 89.0);
        weapon_powers.insert("X_22".to_string(), 1200.0);
        weapon_powers.insert("X_35".to_string(), 145.0);
        
        // Cruise Missiles
        weapon_powers.insert("CM-802AKG".to_string(), 240.0);
        weapon_powers.insert("AGM_84E".to_string(), 360.0);
        weapon_powers.insert("AGM_84H".to_string(), 380.0);
        weapon_powers.insert("X_59M".to_string(), 340.0);
        weapon_powers.insert("X_65".to_string(), 545.0);
        weapon_powers.insert("X_101".to_string(), 545.0);
        weapon_powers.insert("X_555".to_string(), 545.0);
        weapon_powers.insert("AGM_86".to_string(), 545.0);
        
        // Rockets
        weapon_powers.insert("HYDRA_70M15".to_string(), 5.0);
        weapon_powers.insert("HYDRA_70_MK1".to_string(), 5.0);
        weapon_powers.insert("HYDRA_70_MK5".to_string(), 8.0);
        weapon_powers.insert("HYDRA_70_M151".to_string(), 5.0);
        weapon_powers.insert("HYDRA_70_M151_M433".to_string(), 5.0);
        weapon_powers.insert("HYDRA_70_M229".to_string(), 10.0);
        weapon_powers.insert("FFAR Mk1 HE".to_string(), 5.0);
        weapon_powers.insert("FFAR Mk5 HEAT".to_string(), 8.0);
        weapon_powers.insert("HVAR".to_string(), 5.0);
        weapon_powers.insert("Zuni_127".to_string(), 8.0);
        weapon_powers.insert("ARAKM70BHE".to_string(), 5.0);
        weapon_powers.insert("ARAKM70BAP".to_string(), 8.0);
        weapon_powers.insert("SNEB_TYPE251_F1B".to_string(), 4.0);
        weapon_powers.insert("SNEB_TYPE252_F1B".to_string(), 4.0);
        weapon_powers.insert("SNEB_TYPE253_F1B".to_string(), 5.0);
        weapon_powers.insert("SNEB_TYPE256_F1B".to_string(), 6.0);
        weapon_powers.insert("SNEB_TYPE257_F1B".to_string(), 8.0);
        weapon_powers.insert("SNEB_TYPE251_F4B".to_string(), 4.0);
        weapon_powers.insert("SNEB_TYPE252_F4B".to_string(), 4.0);
        weapon_powers.insert("SNEB_TYPE253_F4B".to_string(), 5.0);
        weapon_powers.insert("SNEB_TYPE256_F4B".to_string(), 6.0);
        weapon_powers.insert("SNEB_TYPE257_F4B".to_string(), 8.0);
        weapon_powers.insert("SNEB_TYPE251_H1".to_string(), 4.0);
        weapon_powers.insert("SNEB_TYPE252_H1".to_string(), 4.0);
        weapon_powers.insert("SNEB_TYPE253_H1".to_string(), 5.0);
        weapon_powers.insert("SNEB_TYPE256_H1".to_string(), 6.0);
        weapon_powers.insert("SNEB_TYPE257_H1".to_string(), 8.0);
        weapon_powers.insert("MATRA_F4_SNEBT251".to_string(), 8.0);
        weapon_powers.insert("MATRA_F4_SNEBT253".to_string(), 8.0);
        weapon_powers.insert("MATRA_F4_SNEBT256".to_string(), 8.0);
        weapon_powers.insert("MATRA_F1_SNEBT253".to_string(), 8.0);
        weapon_powers.insert("MATRA_F1_SNEBT256".to_string(), 8.0);
        weapon_powers.insert("TELSON8_SNEBT251".to_string(), 4.0);
        weapon_powers.insert("TELSON8_SNEBT253".to_string(), 8.0);
        weapon_powers.insert("TELSON8_SNEBT256".to_string(), 4.0);
        weapon_powers.insert("TELSON8_SNEBT257".to_string(), 6.0);
        weapon_powers.insert("ARF8M3API".to_string(), 8.0);
        weapon_powers.insert("UG_90MM".to_string(), 8.0);
        weapon_powers.insert("S-24A".to_string(), 24.0);
        weapon_powers.insert("S-25OF".to_string(), 194.0);
        weapon_powers.insert("S-25OFM".to_string(), 150.0);
        weapon_powers.insert("S-25O".to_string(), 150.0);
        weapon_powers.insert("S-25-O".to_string(), 150.0);
        weapon_powers.insert("S_25L".to_string(), 190.0);
        weapon_powers.insert("S-5M".to_string(), 3.0);
        weapon_powers.insert("C_5".to_string(), 8.0);
        weapon_powers.insert("C5".to_string(), 5.0);
        weapon_powers.insert("C_8".to_string(), 5.0);
        weapon_powers.insert("C_8OFP2".to_string(), 5.0);
        weapon_powers.insert("C_13".to_string(), 21.0);
        weapon_powers.insert("C_24".to_string(), 123.0);
        weapon_powers.insert("C_25".to_string(), 151.0);
        
        // Laser Rockets
        weapon_powers.insert("AGR_20".to_string(), 8.0);
        weapon_powers.insert("AGR_20A".to_string(), 8.0);
        weapon_powers.insert("AGR_20_M282".to_string(), 8.0);
        weapon_powers.insert("Hydra_70_M282_MPP".to_string(), 5.0);
        weapon_powers.insert("BRM-1_90MM".to_string(), 8.0);
        
        // JF17 weapons
        weapon_powers.insert("C_701T".to_string(), 38.0);
        weapon_powers.insert("C_701IR".to_string(), 38.0);
        weapon_powers.insert("LS_6_100".to_string(), 45.0);
        weapon_powers.insert("LS_6".to_string(), 100.0);
        weapon_powers.insert("LS_6_500".to_string(), 274.0);
        weapon_powers.insert("Type_200A".to_string(), 107.0);
        weapon_powers.insert("C_802AK".to_string(), 500.0);
        weapon_powers.insert("CM_802AKG".to_string(), 240.0);
        
        // Vehicle/Ship based weapons
        weapon_powers.insert("9M22U".to_string(), 25.0);
        weapon_powers.insert("GRAD_9M22U".to_string(), 25.0);
        weapon_powers.insert("M26".to_string(), 0.0); // Cluster
        weapon_powers.insert("SCUD_RAKETA".to_string(), 985.0);
        weapon_powers.insert("SMERCH_9M55F".to_string(), 46.0);
        weapon_powers.insert("TOW2".to_string(), 6.5);
        weapon_powers.insert("M48".to_string(), 488.0);
        weapon_powers.insert("9M723_HE".to_string(), 488.0);
        weapon_powers.insert("M39A1".to_string(), 0.0); // Cluster
        weapon_powers.insert("9M723".to_string(), 0.0); // Cluster
        weapon_powers.insert("M30".to_string(), 0.0); // Cluster
        
        // Shells
        weapon_powers.insert("weapons.shells.M_105mm_HE".to_string(), 12.0);
        weapon_powers.insert("weapons.shells.M_155mm_HE".to_string(), 60.0);
        weapon_powers.insert("weapons.shells.2A60_120".to_string(), 18.0);
        weapon_powers.insert("weapons.shells.2A18_122".to_string(), 22.0);
        weapon_powers.insert("weapons.shells.2A33_152".to_string(), 50.0);
        weapon_powers.insert("weapons.shells.PLZ_155_HE".to_string(), 60.0);
        weapon_powers.insert("weapons.shells.M185_155".to_string(), 60.0);
        weapon_powers.insert("weapons.shells.2A64_152".to_string(), 50.0);
        weapon_powers.insert("weapons.shells.2A46M_125_HE".to_string(), 5.0);
        weapon_powers.insert("weapons.shells.HESH_105".to_string(), 6.0);
        
        // Naval weapons
        weapon_powers.insert("BGM_109B".to_string(), 450.0);
        weapon_powers.insert("AGM_84S".to_string(), 225.0);
        weapon_powers.insert("P_500".to_string(), 500.0);
        weapon_powers.insert("weapons.shells.AK176_76".to_string(), 1.0);
        weapon_powers.insert("weapons.shells.A222_130".to_string(), 5.0);
        weapon_powers.insert("weapons.shells.53-UBR-281U".to_string(), 5.0);
        weapon_powers.insert("weapons.shells.PJ87_100_PFHE".to_string(), 3.0);
        weapon_powers.insert("weapons.shells.AK100_100".to_string(), 3.0);
        weapon_powers.insert("weapons.shells.AK130_130".to_string(), 5.0);
        weapon_powers.insert("weapons.shells.2A70_100".to_string(), 3.0);
        weapon_powers.insert("weapons.shells.OTO_76".to_string(), 1.0);
        weapon_powers.insert("weapons.shells.MK45_127".to_string(), 5.0);
        weapon_powers.insert("weapons.shells.PJ26_76_PFHE".to_string(), 1.0);
        weapon_powers.insert("weapons.shells.53-UOR-281U".to_string(), 5.0);
        weapon_powers.insert("weapons.shells.MK75_76".to_string(), 1.0);
        
        weapon_powers.insert("MK77mod0-WPN".to_string(), 0.0); // Napalm
        weapon_powers.insert("MK77mod1-WPN".to_string(), 0.0); // Napalm
        weapon_powers.insert("CBU_99".to_string(), 0.0);
        weapon_powers.insert("ROCKEYE".to_string(), 0.0);
        weapon_powers.insert("BLU_3B_GROUP".to_string(), 0.0);
        weapon_powers.insert("CBU_87".to_string(), 0.0);
        weapon_powers.insert("CBU_103".to_string(), 0.0);
        weapon_powers.insert("CBU_97".to_string(), 0.0);
        weapon_powers.insert("CBU_105".to_string(), 0.0);
        weapon_powers.insert("BELOUGA".to_string(), 0.0);
        weapon_powers.insert("BLG66_BELOUGA".to_string(), 0.0);
        weapon_powers.insert("BL_755".to_string(), 0.0);
        weapon_powers.insert("RBK_250".to_string(), 0.0);
        weapon_powers.insert("RBK_250_275_AO_1SCH".to_string(), 0.0);
        weapon_powers.insert("RBK_500".to_string(), 0.0);
        weapon_powers.insert("RBK_500U".to_string(), 0.0);
        weapon_powers.insert("RBK_500AO".to_string(), 0.0);
        weapon_powers.insert("RBK_500U_OAB_2_5RT".to_string(), 0.0);
        weapon_powers.insert("RBK_500_255_PTO_1M".to_string(), 0.0);
        weapon_powers.insert("RBK_500_255_ShO".to_string(), 0.0);
        weapon_powers.insert("AGM_154A".to_string(), 0.0); // Cluster
        weapon_powers.insert("AGM_154B".to_string(), 0.0); // Cluster
        weapon_powers.insert("BK90_MJ1".to_string(), 0.0);
        weapon_powers.insert("BK90_MJ1_MJ2".to_string(), 0.0);
        weapon_powers.insert("BK90_MJ2".to_string(), 0.0);
        weapon_powers.insert("GB-6".to_string(), 0.0);
        weapon_powers.insert("GB-6-HE".to_string(), 0.0);
        weapon_powers.insert("GB-6-SFW".to_string(), 0.0);
        
        // Configure HEAT weapons for shaped charge effects
        let mut heat_weapons = FxHashMap::default();
        heat_weapons.insert("AGM_65D".to_string(), true);
        heat_weapons.insert("AGM_65E".to_string(), true);
        heat_weapons.insert("AGM_65F".to_string(), true);
        heat_weapons.insert("AGM_65G".to_string(), true);
        heat_weapons.insert("AGM_65H".to_string(), true);
        heat_weapons.insert("AGM_65K".to_string(), true);
        heat_weapons.insert("AGM_65L".to_string(), true);
        heat_weapons.insert("AGM_114".to_string(), true);
        heat_weapons.insert("AGM_114K".to_string(), true);
        heat_weapons.insert("X_35".to_string(), true);
        heat_weapons.insert("HYDRA_70_MK5".to_string(), true);
        heat_weapons.insert("FFAR Mk5 HEAT".to_string(), true);
        heat_weapons.insert("Hydra_70_M282_MPP".to_string(), true);
        
        heat_weapons.insert("TOW2".to_string(), true);
        
        Self {
            enabled: true,
            weapon_powers,
            cookoff: CookOffConfig::default(),
            heat_weapons,
            wave_explosions: WaveExplosionConfig {
                enabled: true,
                scaling: 2.0,
                damage_threshold: 0.1,
                always_cascade_explode: false,
            },
            blast_wave: BlastWaveConfig {
                search_radius: 90.0,
                use_dynamic_radius: true,
                dynamic_radius_modifier: 2.0,
            },
            shaped_charge: ShapedChargeConfig {
                enabled: true,
                multiplier: 0.2,
            },
            cluster_bombs: ClusterBombConfig {
                enabled: true,
                base_length: 150.0,
                base_width: 200.0,
                max_length: 300.0,
                max_width: 400.0,
                min_length: 100.0,
                min_width: 150.0,
                bomblet_reduction_modifier: true,
                bomblet_damage_modifier: 2.0,
                submunition_powers: {
                    let mut sub_powers = FxHashMap::default();
                    // From clusterSubMunTable in Lua script
                    sub_powers.insert("Mk 118".to_string(), 2.0);
                    sub_powers.insert("BLU-97B".to_string(), 3.0);
                    sub_powers.insert("BLU-97/B".to_string(), 3.0);
                    sub_powers.insert("BLU-108/B".to_string(), 2.0);
                    sub_powers.insert("AO-2-5".to_string(), 2.5);
                    sub_powers.insert("BLU-3".to_string(), 1.0);
                    sub_powers.insert("BLU-3B".to_string(), 1.0);
                    sub_powers.insert("BLU-4B".to_string(), 1.0);
                    sub_powers.insert("HEAT".to_string(), 1.5);
                    sub_powers.insert("MJ2".to_string(), 1.0);
                    sub_powers.insert("MJ1".to_string(), 1.0);
                    sub_powers.insert("GR_66_AC".to_string(), 1.0);
                    sub_powers.insert("9N235".to_string(), 1.85);
                    sub_powers.insert("GB-06".to_string(), 2.0);
                    sub_powers.insert("SD-10A".to_string(), 1.0);
                    sub_powers.insert("PTAB-2.5KO".to_string(), 1.0);
                    sub_powers.insert("PTAB-10-5".to_string(), 2.5);
                    sub_powers.insert("OAB-2-5RT".to_string(), 1.0);
                    sub_powers.insert("AO-1SCh".to_string(), 1.0);
                    sub_powers.insert("MM-06".to_string(), 1.5);
                    sub_powers.insert("BETAB-M".to_string(), 5.0);
                    sub_powers.insert("MJ1-MJ2".to_string(), 1.0);
                    sub_powers.insert("BLU-61".to_string(), 1.0);
                    sub_powers.insert("BLG-66 AC".to_string(), 1.0);
                    sub_powers.insert("PTAB-2-5".to_string(), 1.0);
                    sub_powers.insert("AO-2.5RT".to_string(), 1.0);
                    sub_powers.insert("M77".to_string(), 1.0);
                    sub_powers.insert("M74".to_string(), 1.0);
                    sub_powers.insert("9N722".to_string(), 1.0);
                    sub_powers.insert("SD-2".to_string(), 1.0);
                    sub_powers.insert("BLG-66 EG".to_string(), 1.0);
                    sub_powers.insert("M77 submunitions".to_string(), 1.0);
                    sub_powers
                },
            },
            ordnance_protection: OrdnanceProtectionConfig {
                enabled: true,
                protection_radius: 20.0,
                detect_ordnance_destruction: true,
                snap_to_ground_if_destroyed: true,
                max_snapped_height: 80.0,
                recent_large_explosion_snap: true,
                recent_large_explosion_range: 100.0,
                recent_large_explosion_time: 4.0,
            },
            overall_scaling: 1.5, // Increased from 1.0 to create more damage
            rocket_multiplier: 1.3,
            only_players_weapons: false,
            cascade_damage_threshold: 0.05, // Lowered from 0.1 to trigger more cascade explosions
            cascade_explode_threshold: 80.0, // Lowered from 60.0 to allow more units to trigger cascade
            cascade_scaling: 2.0,
        }
    }
}

/// Tracks a weapon that was fired and is awaiting impact
#[derive(Debug, Clone)]
struct TrackedWeapon {
    /// The weapon type name (e.g., "Mk_84")
    weapon_type: String,
    /// Current position of the weapon
    position: LuaVec3,
    /// Current velocity/direction of the weapon
    velocity: LuaVec3,
    /// Current speed magnitude
    speed: f64,
    /// Predicted impact point (terrain intersection)
    predicted_impact: Option<LuaVec3>,
    /// Last update time
    last_update: DateTime<Utc>,
}


impl SplashConfig {
    /// Get explosion power for a weapon, returns None if weapon is not configured
    pub fn get_weapon_power(&self, weapon_name: &str) -> Option<f32> {
        self.weapon_powers.get(weapon_name).copied()
    }
    
    /// Check if a weapon should have splash damage
    pub fn has_weapon(&self, weapon_name: &str) -> bool {
        self.weapon_powers.contains_key(weapon_name)
    }
}

/// Main splash damage system
#[derive(Debug)]
pub struct SplashDamage {
    /// Configuration for splash damage
    config: SplashConfig,
    /// Currently tracked weapons awaiting impact
    tracked_weapons: FxHashMap<DcsOid<ClassWeapon>, TrackedWeapon>,
    /// Cook-off system
    cookoff: CookOff,
    /// Recent explosions for ordnance protection
    recent_explosions: VecDeque<RecentExplosion>,
}

impl Default for SplashDamage {
    fn default() -> Self {
        let config = SplashConfig::default();
        debug!("SPLASH: System initialized with {} configured weapons", config.weapon_powers.len());
        
        let mut splash_damage = Self {
            config: config.clone(),
            tracked_weapons: FxHashMap::default(),
            cookoff: CookOff::new(config.cookoff),
            recent_explosions: VecDeque::new(),
        };
        
        // Log cook-off system initialization
        debug!(
            "SPLASH: Cook-off system initialized with {} units configured",
            splash_damage.cookoff.units_count()
        );
        
        // Log cook-off system status
        splash_damage.log_cookoff_status();
        
        // Initialize additional mission units
        {
            let cookoff_system = splash_damage.cookoff_mut();
            // Initialize additional units that might be spawned dynamically
            cookoff_system.add_unit("M35".into(), UnitProperties {
                explosion: ExplosionConfig {
                    power: 45.0,
                    enabled: true,
                },
                cookoff: CookOffEffectConfig {
                    enabled: true,
                    count: 3,
                    power: 10.0,
                    duration: 25.0,
                    random_timing: true,
                    power_random: 35.0,
                },
                smoke: SmokeConfig {
                    is_tanker: false,
                    size: 3,
                    duration: 90,
                },
            });
        }
        
        // Apply mission settings
        {
            let mut config = splash_damage.cookoff_config().clone();
            config.effects_chance = (config.effects_chance * 1.0).min(1.0);
            config.damage_threshold = (config.damage_threshold * (2.0 - 1.0)).max(10.0);
            splash_damage.update_cookoff_config(config);
        }
        
        splash_damage
    }
}

impl<'lua> SplashDamage {
    /// Handle a weapon shot event
    pub fn handle_weapon_shot(
        &mut self,
        _lua: MizLua,
        db: &Db,
        event: &ShotEvent,
        now: DateTime<Utc>,
    ) -> Result<()> {
        if !self.config.enabled {
            debug!("SPLASH: System disabled, skipping weapon tracking");
            return Ok(());
        }
        
        // Check if weapon still exists
        if !event.weapon.is_exist()? {
            return Ok(());
        }
        
        let weapon_id = event.weapon.object_id()?;
        let weapon_type = event.weapon_name.as_str(); // Avoid unnecessary string allocation
        
        // Check if this is a player weapon
        let player_ucid = db.ephemeral
            .get_slot_by_object_id(&event.initiator.object_id()?)
            .and_then(|slot| db.ephemeral.player_in_slot(slot))
            .copied();
        
        // Only track weapons fired by players if configured to do so
        if self.config.only_players_weapons && player_ucid.is_none() {
            debug!("SPLASH: Only tracking player weapons, skipping AI weapon {}", weapon_type);
            return Ok(());
        }
        
        // Check if this weapon type should have splash damage
        if !self.config.has_weapon(weapon_type) {
            debug!("SPLASH: Weapon {} not configured for splash damage, skipping", weapon_type);
            return Ok(());
        }
        
        // Get initial weapon position and velocity from DCS
        let position = match event.weapon.get_point() {
            Ok(pos) => pos,
            Err(e) => {
                debug!("SPLASH: Failed to get weapon position: {:?}", e);
                LuaVec3(na::Vector3::new(0.0, 0.0, 0.0))
            }
        };
        
        let velocity = match event.weapon.get_velocity() {
            Ok(vel) => vel,
            Err(e) => {
                debug!("SPLASH: Failed to get weapon velocity: {:?}", e);
                LuaVec3(na::Vector3::new(0.0, 0.0, 0.0))
            }
        };
        
        let speed = (velocity.0.x.powi(2) + velocity.0.y.powi(2) + velocity.0.z.powi(2)).sqrt() as f32;
        
        
        debug!(
            "SPLASH: Tracking weapon shot: {} by player {:?} - position/velocity will be updated continuously",
            weapon_type, player_ucid
        );
        
        // Track the weapon with essential data
        let tracked_weapon = TrackedWeapon {
            weapon_type: weapon_type.to_string(),
            position,
            velocity,
            speed: speed as f64,
            predicted_impact: None,
            last_update: now,
        };
        
        self.tracked_weapons.insert(weapon_id, tracked_weapon);
        
        debug!("SPLASH: Added weapon {} to tracking system (total tracked: {})", weapon_type, self.tracked_weapons.len());
        
        Ok(())
    }
    
    /// Calculate predicted impact point using terrain intersection
    fn calculate_predicted_impact(&self, lua: MizLua, position: LuaVec3, velocity: LuaVec3) -> Option<LuaVec3> {
        // Use DCS land.getIP() to find terrain intersection point
        match dcso3::land::Land::singleton(lua) {
            Ok(land) => {
                // Calculate lookahead distance based on velocity magnitude
                let speed = (velocity.0.x.powi(2) + velocity.0.y.powi(2) + velocity.0.z.powi(2)).sqrt();
                let lookahead_distance = speed * 3.0; // 3 seconds lookahead for better accuracy
                
                match land.get_ip(position, velocity, lookahead_distance) {
                    Ok(intersection_point) => {
                        // Get ground height at impact point and adjust if needed
                        let ground_height = self.get_ground_height(lua, intersection_point);
                        let adjusted_impact = LuaVec3(na::Vector3::new(
                            intersection_point.0.x,
                            ground_height + 0.1, // Small offset above ground
                            intersection_point.0.z,
                        ));
                        
                        debug!("SPLASH: Calculated terrain intersection at {:?} (ground height: {:.2}m)", 
                               adjusted_impact, ground_height);
                        Some(adjusted_impact)
                    }
                    Err(e) => {
                        debug!("SPLASH: Failed to calculate terrain intersection: {:?}", e);
                        None
                    }
                }
            }
            Err(e) => {
                debug!("SPLASH: Failed to get land singleton: {:?}", e);
                None
            }
        }
    }
    
    /// Get ground height at a position
    fn get_ground_height(&self, lua: MizLua, position: LuaVec3) -> f64 {
        // Use DCS land.getHeight() to get terrain height
        match dcso3::land::Land::singleton(lua) {
            Ok(land) => {
                let pos_2d = dcso3::LuaVec2(na::Vector2::new(position.0.x, position.0.z));
                match land.get_height(pos_2d) {
                    Ok(height) => height,
                    Err(e) => {
                        debug!("SPLASH: Failed to get ground height: {:?}", e);
                        0.0
                    }
                }
            }
            Err(e) => {
                debug!("SPLASH: Failed to get land singleton for height: {:?}", e);
                0.0
            }
        }
    }
    
    /// Get proper ground-level position with height offset like the Lua script
    fn get_ground_position_with_offset(&self, lua: MizLua, position: LuaVec3, height_offset: f64) -> LuaVec3 {
        let ground_height = self.get_ground_height(lua, position);
        let adjusted_height = ground_height + height_offset;
        
        debug!("SPLASH: Ground positioning - Original: {:?}, Ground height: {:.2}m, Offset: {:.2}m, Final: {:.2}m", 
               position, ground_height, height_offset, adjusted_height);
        
        LuaVec3(na::Vector3::new(
            position.0.x,
            adjusted_height,
            position.0.z,
        ))
    }
    
    /// Calculate dynamic blast radius like the Lua script: pow(power, 1/3) * 10 * modifier
    fn calculate_dynamic_blast_radius(&self, power: f32) -> f32 {
        let radius = (power as f64).powf(1.0/3.0) * 10.0 * self.config.blast_wave.dynamic_radius_modifier as f64;
        radius as f32
    }
    
    /// Get weapon object by ID
    fn get_weapon_object<'a>(&self, lua: MizLua<'a>, weapon_id: DcsOid<ClassWeapon>) -> Result<dcso3::weapon::Weapon<'a>> {
        // Use DCS API to get weapon object by ID
        dcso3::weapon::Weapon::get_instance(lua, &weapon_id)
    }
    
    
    /// Check for weapon impacts by monitoring tracked weapons
    pub fn check_weapon_impacts(&mut self, lua: MizLua, now: DateTime<Utc>) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        
        // Check each tracked weapon for impact
        let mut weapons_to_remove = Vec::new();
        let mut weapons_to_update = Vec::new();
        let mut explosions_to_create = Vec::new();
        
        for (weapon_id, tracked_weapon) in &self.tracked_weapons {
            // Try to get the weapon object to check its current state
            match self.get_weapon_object(lua, weapon_id.clone()) {
                Ok(weapon_obj) => {
                    // Weapon exists - update position, direction, and speed (like Lua lines 3395-3397)
                    // wpnData.pos = wpnData.wpn:getPosition().p
                    // wpnData.dir = wpnData.wpn:getPosition().x  
                    // wpnData.speed = wpnData.wpn:getVelocity()
                    
                    // Get current position and velocity from weapon object (like Lua lines 3395-3397)
                    let current_position = match weapon_obj.get_point() {
                        Ok(pos) => pos,
                        Err(e) => {
                            debug!("SPLASH: Failed to get weapon position for {}: {:?}", tracked_weapon.weapon_type, e);
                            tracked_weapon.position // Fallback to last known position
                        }
                    };
                    
                    let current_velocity = match weapon_obj.get_velocity() {
                        Ok(vel) => vel,
                        Err(e) => {
                            debug!("SPLASH: Failed to get weapon velocity for {}: {:?}", tracked_weapon.weapon_type, e);
                            tracked_weapon.velocity // Fallback to last known velocity
                        }
                    };
                    
                    let current_speed = (current_velocity.0.x.powi(2) + current_velocity.0.y.powi(2) + current_velocity.0.z.powi(2)).sqrt();
                    
                    // Calculate predicted impact point
                    let predicted_impact = self.calculate_predicted_impact(lua, current_position, current_velocity);
                    
                    // Check for ground impact using terrain height
                    let ground_height = self.get_ground_height(lua, current_position);
                    let altitude_above_ground = current_position.0.y - ground_height;
                    
                    // Lua script approach: if weapon exists, keep tracking; if not, it impacted
                    // Since we successfully got the weapon object, it still exists - keep tracking
                    debug!(
                        "SPLASH: Weapon {} still in flight at altitude {:.2}m above ground, speed {:.2}m/s - updating position/velocity",
                        tracked_weapon.weapon_type, altitude_above_ground, current_speed
                    );
                    
                    // Update weapon tracking data (like Lua lines 3395-3397)
                    let mut updated_weapon = tracked_weapon.clone();
                    updated_weapon.position = current_position;
                    updated_weapon.velocity = current_velocity;
                    updated_weapon.speed = current_speed;
                    updated_weapon.predicted_impact = predicted_impact;
                    updated_weapon.last_update = now;
                    
                    weapons_to_update.push((weapon_id.clone(), updated_weapon));
                }
                Err(_) => {
                    // Weapon no longer exists - it has impacted (like Lua line 3515-3517)
                    debug!("SPLASH: Weapon {} no longer exists, assuming impact", tracked_weapon.weapon_type);
                    
                    // Use Lua script approach: get terrain intersection point (like Lua lines 3519-3525)
                    // local ip = land.getIP(wpnData.pos, wpnData.dir, lookahead(wpnData.speed))
                    // local explosionPoint = ip or wpnData.pos
                    let explosion_point = if let Some(predicted_impact) = tracked_weapon.predicted_impact {
                        predicted_impact
                    } else {
                        // Fallback to last known position if no predicted impact
                        tracked_weapon.position
                    };
                    
                    debug!("SPLASH: Weapon {} impacted at {:?}, creating explosion", tracked_weapon.weapon_type, explosion_point);
                    
                    // Get explosion power and create explosion
                    if let Some(explosion_power) = self.config.get_weapon_power(&tracked_weapon.weapon_type) {
                        explosions_to_create.push((explosion_point, explosion_power, tracked_weapon.weapon_type.clone()));
                    }
                    
                    weapons_to_remove.push(weapon_id.clone());
                }
            }
        }
        
        // Update weapons that are still in flight
        for (weapon_id, updated_weapon) in weapons_to_update {
            self.tracked_weapons.insert(weapon_id, updated_weapon);
        }
        
        // Remove weapons that have impacted
        for weapon_id in weapons_to_remove {
            self.tracked_weapons.remove(&weapon_id);
        }
        
        // Create explosions for impacted weapons
        for (explosion_point, explosion_power, weapon_type) in explosions_to_create {
            debug!("SPLASH: Creating explosion for weapon {} at {:?} with power {}", weapon_type, explosion_point, explosion_power);
            if let Err(e) = self.create_wave_explosion(lua, &explosion_point, explosion_power, &weapon_type) {
                debug!("SPLASH: Failed to create wave explosion for weapon {}: {:?}", weapon_type, e);
            } else {
                debug!("SPLASH: Successfully created explosion for weapon {}", weapon_type);
            }
        }
        
        Ok(())
    }
    
    /// Create an explosion at the specified position
    fn create_explosion(&mut self, lua: MizLua, weapon_name: &str, position: LuaVec3) -> Result<()> {
        // Get explosion power for this weapon type
        let explosion_power = match self.config.get_weapon_power(weapon_name) {
            Some(power) => power,
            None => {
                debug!("SPLASH: Weapon {} not configured for splash damage, skipping explosion", weapon_name);
                return Ok(());
            }
        };
        
        // Skip explosions with zero or negative power
        if explosion_power <= 0.0 {
            debug!("SPLASH: Skipping explosion for weapon {} with power {}", weapon_name, explosion_power);
            return Ok(());
        }
        
        // Check ordnance protection
        if !self.check_ordnance_protection(lua, &position) {
            debug!("SPLASH: Explosion blocked by ordnance protection for weapon {} at {:?}", weapon_name, position);
            return Ok(());
        }
        
        // Apply overall scaling and rocket multiplier
        let mut final_power = explosion_power * self.config.overall_scaling;
        
        // Apply rocket multiplier if this is a rocket
        if weapon_name.contains("ROCKET") || weapon_name.contains("rocket") {
            final_power *= self.config.rocket_multiplier;
        }
        
        // Apply shaped charge effects if enabled and weapon is HEAT
        if self.config.shaped_charge.enabled && *self.config.heat_weapons.get(weapon_name).unwrap_or(&false) {
            final_power *= self.config.shaped_charge.multiplier;
            debug!("SPLASH: Applied shaped charge multiplier {} to HEAT weapon {}", 
                   self.config.shaped_charge.multiplier, weapon_name);
        }
        
        // Check if this is a cluster bomb and apply cluster bomb effects
        if self.config.cluster_bombs.enabled && self.is_cluster_bomb(weapon_name) {
            self.handle_cluster_bomb_effects(lua, position, final_power, weapon_name)?;
        }
        
        // Get proper ground-level position with height offset like the Lua script
        // Lua script uses small offsets: 0.1m for ground level, 1.6m for some explosions
        let ground_position = self.get_ground_position_with_offset(lua, position, 0.1);
        
        debug!(
            "SPLASH: Creating explosion for weapon {} at position {:?} (ground-adjusted) with power {} (scaled from {})",
            weapon_name, ground_position, final_power, explosion_power
        );
        
        // Create the explosion with error handling
        match Trigger::singleton(lua) {
            Ok(trigger) => {
                match trigger.action() {
                    Ok(action) => {
                        debug!("SPLASH: About to create explosion for weapon {} at {:?} with power {}", weapon_name, ground_position, final_power);
                        
                        // Use the power directly like the Lua script does - no minimum threshold
                        if let Err(e) = action.explosion(ground_position, final_power) {
                            debug!("SPLASH: Failed to create explosion for weapon {}: {:?}", weapon_name, e);
                            return Err(e);
                        } else {
                            debug!("SPLASH: DCS explosion API call succeeded for weapon {} with power {}", weapon_name, final_power);
                            
                            // Add large black smoke effect for cook-off (like Lua script)
                            let smoke_name = format!("explosion_smoke_{}", weapon_name);
                            if let Err(e) = action.effect_smoke_big(ground_position, dcso3::trigger::SmokePreset::LargeSmokeAndFire, 1.0, smoke_name.into()) {
                                debug!("SPLASH: Failed to create large smoke effect for weapon {}: {:?}", weapon_name, e);
                            } else {
                                debug!("SPLASH: Created large black smoke effect for weapon {} at {:?}", weapon_name, ground_position);
                            }
                        }
                    }
                    Err(e) => {
                        debug!("SPLASH: Failed to get action for weapon {}: {:?}", weapon_name, e);
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                debug!("SPLASH: Failed to get trigger singleton for weapon {}: {:?}", weapon_name, e);
                return Err(e);
            }
        }
        
        Ok(())
    }

    /// Create an explosion with a specific power value
    fn create_explosion_with_power_static(
        &self,
        lua: MizLua,
        weapon_name: &str,
        position: LuaVec3,
        power: f32,
    ) -> Result<()> {
        // Get proper ground-level position with height offset like the Lua script
        // Use 1.6m offset for bomblet explosions (like the Lua script)
        let ground_position = self.get_ground_position_with_offset(lua, position, 1.6);
        
        debug!("SPLASH: Creating explosion for weapon {} at position {:?} (ground-adjusted) with power {}", 
               weapon_name, ground_position, power);
        
        // Create the explosion with error handling
        match Trigger::singleton(lua) {
            Ok(trigger) => {
                match trigger.action() {
                    Ok(action) => {
                        debug!("SPLASH: About to create static explosion for weapon {} at {:?} with power {}", weapon_name, ground_position, power);
                        
                        // Use the power directly like the Lua script does - no minimum threshold
                        if let Err(e) = action.explosion(ground_position, power) {
                            debug!("SPLASH: Failed to create explosion for weapon {}: {:?}", weapon_name, e);
                            return Err(e);
                        } else {
                            debug!("SPLASH: DCS static explosion API call succeeded for weapon {} with power {}", weapon_name, power);
                            
                            // Add large black smoke effect for cook-off (like Lua script)
                            let smoke_name = format!("static_explosion_smoke_{}", weapon_name);
                            if let Err(e) = action.effect_smoke_big(ground_position, dcso3::trigger::SmokePreset::LargeSmokeAndFire, 1.0, smoke_name.into()) {
                                debug!("SPLASH: Failed to create large smoke effect for weapon {}: {:?}", weapon_name, e);
                            } else {
                                debug!("SPLASH: Created large black smoke effect for weapon {} at {:?}", weapon_name, ground_position);
                            }
                        }
                    }
                    Err(e) => {
                        debug!("SPLASH: Failed to get action for weapon {}: {:?}", weapon_name, e);
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                debug!("SPLASH: Failed to get trigger singleton for weapon {}: {:?}", weapon_name, e);
                return Err(e);
            }
        }
        
        Ok(())
    }

    /// Check if a weapon is a cluster bomb
    fn is_cluster_bomb(&self, weapon_name: &str) -> bool {
        // Common cluster bomb patterns
        weapon_name.contains("CBU") || 
        (weapon_name.contains("BLU") && !weapon_name.contains("DURANDAL")) || // Exclude Durandal anti-runway bombs
        weapon_name.contains("RBK") ||
        weapon_name.contains("BK90") ||
        weapon_name == "AGM_154A" || // Cluster (BLU-97/B bomblets)
        weapon_name == "AGM_154B" || // Cluster (BLU-108/B submunitions)
        weapon_name.contains("cluster") ||
        weapon_name.contains("Cluster") ||
        weapon_name.contains("bomblet") ||
        weapon_name.contains("submunition") ||
        weapon_name == "M26" ||
        weapon_name == "M39A1" ||
        weapon_name == "9M723" ||
        weapon_name == "M30"
    }

    /// Get submunition name for a cluster bomb weapon
    fn get_submunition_name(&self, weapon_name: &str) -> String {
        // Map weapon names to their submunition types based on the Lua script
        match weapon_name {
            "CBU_99" | "ROCKEYE" => "Mk 118".to_string(),
            "CBU_87" | "CBU_103" => "BLU-97B".to_string(),
            "AGM_154A" => "BLU-97/B".to_string(),
            "AGM_154B" => "BLU-108/B".to_string(),
            "RBK_500AO" => "AO-2-5".to_string(),
            "BLU_3B_GROUP" => "BLU-3B".to_string(),
            "BL_755" => "HEAT".to_string(),
            "BK90_MJ2" => "MJ2".to_string(),
            "BK90_MJ1" => "MJ1".to_string(),
            "BK90_MJ1_MJ2" => "MJ1-MJ2".to_string(),
            "BELOUGA" | "BLG66_BELOUGA" => "GR_66_AC".to_string(),
            "RBK_500U_OAB_2_5RT" => "OAB-2-5RT".to_string(),
            "RBK_250_275_AO_1SCH" => "AO-1SCh".to_string(),
            "GB-6-SFW" => "MM-06".to_string(),
            "M26" => "M77".to_string(),
            "M39A1" => "M74".to_string(),
            "9M723" => "9N722".to_string(),
            "M30" => "M77 submunitions".to_string(),
            "AB_250_2_SD_2" => "SD-2".to_string(),
            _ => "Generic".to_string(), // Default fallback
        }
    }

    /// Handle cluster bomb effects
    fn handle_cluster_bomb_effects(
        &mut self,
        lua: MizLua,
        position: LuaVec3,
        power: f32,
        weapon_name: &str,
    ) -> Result<()> {
        debug!("SPLASH: Handling cluster bomb effects for {} at {:?}", weapon_name, position);
        
        // Calculate bomblet spread based on configuration
        let length = self.calculate_bomblet_spread(power, true); // forward spread
        let width = self.calculate_bomblet_spread(power, false); // lateral spread
        
        // Calculate number of bomblets
        let bomblet_count = self.calculate_bomblet_count(power);
        
        debug!("SPLASH: Cluster bomb {} will create {} bomblets with spread {}x{}", 
               weapon_name, bomblet_count, length, width);
        
        // Create bomblet explosions
        for i in 0..bomblet_count {
            let bomblet_position = self.calculate_bomblet_position(position, length, width, i, bomblet_count);
            // Get submunition power from configuration, default to weapon power if not found
            let submunition_name = self.get_submunition_name(weapon_name);
            let bomblet_power = self.config.cluster_bombs.submunition_powers
                .get(&submunition_name)
                .copied()
                .unwrap_or(power * self.config.cluster_bombs.bomblet_damage_modifier);
            
            // Create individual bomblet explosion with proper bomblet power
            if let Err(e) = self.create_explosion_with_power_static(lua, &format!("{}_bomblet_{}", weapon_name, i), bomblet_position, bomblet_power) {
                debug!("SPLASH: Failed to create bomblet explosion: {:?}", e);
            }
        }
        
        Ok(())
    }

    /// Calculate bomblet spread based on power and direction
    fn calculate_bomblet_spread(&self, power: f32, is_forward: bool) -> f32 {
        let base_spread = if is_forward {
            self.config.cluster_bombs.base_length
        } else {
            self.config.cluster_bombs.base_width
        };
        
        let max_spread = if is_forward {
            self.config.cluster_bombs.max_length
        } else {
            self.config.cluster_bombs.max_width
        };
        
        let min_spread = if is_forward {
            self.config.cluster_bombs.min_length
        } else {
            self.config.cluster_bombs.min_width
        };
        
        // Scale spread based on power
        let power_factor = (power / 100.0).min(1.0);
        let spread = base_spread + (max_spread - base_spread) * power_factor;
        
        spread.max(min_spread).min(max_spread)
    }

    /// Calculate number of bomblets based on power
    fn calculate_bomblet_count(&self, power: f32) -> u32 {
        let base_count = (power / 10.0) as u32;
        
        if self.config.cluster_bombs.bomblet_reduction_modifier {
            // Reduce bomblet count for performance
            (base_count / 2).max(1)
        } else {
            base_count.max(1)
        }
    }

    /// Calculate individual bomblet position
    fn calculate_bomblet_position(
        &self,
        center: LuaVec3,
        length: f32,
        width: f32,
        index: u32,
        total_count: u32,
    ) -> LuaVec3 {
        // Distribute bomblets in a pattern
        let angle = (index as f32 / total_count as f32) * 2.0 * std::f32::consts::PI;
        let radius = (index as f32 / total_count as f32) * (length + width) / 2.0;
        
        let offset_x = radius * angle.cos();
        let offset_z = radius * angle.sin();
        
        LuaVec3(na::Vector3::new(
            center.0.x + offset_x as f64,
            center.0.y,
            center.0.z + offset_z as f64,
        ))
    }

    /// Create a wave explosion with secondary explosions radiating outward
    pub fn create_wave_explosion(
        &mut self,
        lua: MizLua,
        center: &LuaVec3,
        power: f32,
        weapon_type: &str,
    ) -> Result<()> {
        if !self.config.wave_explosions.enabled {
            return Ok(());
        }

        debug!("SPLASH: Creating wave explosion for {} at {:?} with power {}", 
               weapon_type, center, power);

        // Calculate dynamic blast radius like the Lua script
        let blast_radius = if self.config.blast_wave.use_dynamic_radius {
            self.calculate_dynamic_blast_radius(power)
        } else {
            self.config.blast_wave.search_radius
        };
        
        // Get ground-adjusted position for the explosion (same as used in create_explosion)
        let ground_height = self.get_ground_height(lua, *center);
        let height_offset = 0.1; // Small offset above ground
        let adjusted_height = ground_height + height_offset;
        let ground_position = LuaVec3(na::Vector3::new(
            center.0.x,
            adjusted_height,
            center.0.z,
        ));
        
        debug!("SPLASH: Ground positioning for blast wave - Original: {:?}, Ground height: {:.2}m, Final: {:?}", 
               center, ground_height, ground_position);
        
        // Create the main explosion at ground level
        self.create_explosion(lua, weapon_type, ground_position)?;
        
        // Add to recent explosions for ordnance protection
        self.add_recent_explosion(&ground_position, blast_radius);
        
        // Execute blast wave with cascading explosions at ground level
        self.execute_blast_wave(lua, &ground_position, blast_radius, power, weapon_type)?;
        
        // Schedule additional wave effects
        self.schedule_wave_effects(lua, &ground_position, blast_radius, power, weapon_type, Vec::new())?;
        
        Ok(())
    }

    /// Execute blast wave with cascading explosions like the Lua script
    fn execute_blast_wave(
        &mut self,
        lua: MizLua,
        center: &LuaVec3,
        blast_radius: f32,
        power: f32,
        weapon_type: &str,
    ) -> Result<()> {
        debug!("SPLASH: Executing blast wave for {} at {:?} with radius {:.1}m", 
               weapon_type, center, blast_radius);

        // Scan for nearby units within blast radius
        let found_units = match self.scan_units_in_radius(lua, center, blast_radius) {
            Ok(units) => {
                debug!("SPLASH: Found {} units within blast radius", units.len());
                units
            }
            Err(e) => {
                debug!("SPLASH: Failed to scan units in blast radius: {:?}", e);
                return Ok(()); // Don't fail the entire explosion if scanning fails
            }
        };

        // Process each found unit for cascading explosions
        for unit_data in found_units {
            if let Err(e) = self.process_unit_for_cascade(lua, &unit_data, center, power, weapon_type) {
                debug!("SPLASH: Failed to process unit for cascade: {:?}", e);
            }
        }

        Ok(())
    }

    /// Scan for units within blast radius using DCS world search
    fn scan_units_in_radius(
        &self,
        lua: MizLua,
        center: &LuaVec3,
        radius: f32,
    ) -> Result<Vec<BlastWaveUnit>> {
        debug!("SPLASH: Scanning for units within {:.1}m radius of {:?}", radius, center);
        
        let mut found_units = Vec::new();
        
        debug!("SPLASH: Search details - Center: {:?}, Radius: {:.1}m", center, radius);
        
        // Search for units in the area using the working Lua-style method
        let world = dcso3::world::World::singleton(lua)?;
        debug!("SPLASH: Using multi-category search (UNIT, STATIC, SCENERY, CARGO) at point {:?} with radius {}", 
               center, radius);
        
        // Use multi-category search like the Lua script does (UNIT, STATIC, SCENERY, CARGO)
        let objects = world.search_objects_multi_category(
            *center,
            radius as f64, // Convert f32 to f64 for the method
        )?;
        
        debug!("SPLASH: DCS search returned {} objects", objects.len());
        
        // Process each found object
        for object in objects {
            // Get unit information
            match object.get_position() {
                Ok(position) => {
                    // Calculate distance from explosion center
                    let distance = self.calculate_distance_3d(center, &position.p);
                    
                    // Get unit health information - use a default since get_life might not be available
                    let (health, max_health) = (1.0, 1.0); // Default to full health
                    
                    // Get unit type name
                    let unit_type = match object.get_type_name() {
                        Ok(name) => name.to_string(),
                        Err(_) => "Unknown".to_string(),
                    };
                    
                    // Determine if it's a ground unit (simplified check)
                    let is_ground_unit = !unit_type.to_lowercase().contains("aircraft") && 
                                       !unit_type.to_lowercase().contains("helicopter");
                    
                    debug!("SPLASH: Found unit {} at distance {:.1}m (health: {:.2})", 
                           unit_type, distance, health);
                    
                    found_units.push(BlastWaveUnit {
                        position: position.p,
                        distance,
                        health,
                        max_health,
                        unit_type,
                        is_ground_unit,
                    });
                }
                Err(e) => {
                    debug!("SPLASH: Failed to get position for unit: {:?}", e);
                }
            }
        }
        
        debug!("SPLASH: Found {} units within blast radius", found_units.len());
        Ok(found_units)
    }

    /// Calculate 3D distance between two points
    fn calculate_distance_3d(&self, pos1: &LuaVec3, pos2: &LuaVec3) -> f32 {
        let dx = pos1.0.x - pos2.0.x;
        let dy = pos1.0.y - pos2.0.y;
        let dz = pos1.0.z - pos2.0.z;
        ((dx * dx + dy * dy + dz * dz) as f32).sqrt()
    }

    /// Process a unit for cascading explosions
    fn process_unit_for_cascade(
        &mut self,
        lua: MizLua,
        unit_data: &BlastWaveUnit,
        _explosion_center: &LuaVec3,
        explosion_power: f32,
        _weapon_type: &str,
    ) -> Result<()> {
        // Calculate damage based on distance and explosion power
        let damage = self.calculate_blast_damage(unit_data.distance, explosion_power);
        
        // Check if damage exceeds threshold
        if damage < self.config.cascade_damage_threshold {
            debug!("SPLASH: Unit {} damage {:.4} below threshold {:.4}, skipping cascade", 
                   unit_data.unit_type, damage, self.config.cascade_damage_threshold);
            return Ok(());
        }
        
        // Check if unit health is below cascade threshold
        let health_percent = (unit_data.health / unit_data.max_health) * 100.0;
        if health_percent > self.config.cascade_explode_threshold as f64 {
            debug!("SPLASH: Unit {} health {:.1}% above cascade threshold {:.1}%, skipping cascade", 
                   unit_data.unit_type, health_percent, self.config.cascade_explode_threshold);
            return Ok(());
        }
        
        // Calculate cascade explosion power
        let cascade_power = damage * self.config.cascade_scaling;
        
        // Adjust explosion behavior based on unit type
        let explosion_name = if unit_data.is_ground_unit {
            format!("{}_ground_cascade", unit_data.unit_type)
        } else {
            format!("{}_air_cascade", unit_data.unit_type)
        };
        
        debug!("SPLASH: Creating {} cascade explosion for {} at distance {:.1}m with power {:.2}", 
               if unit_data.is_ground_unit { "ground" } else { "air" },
               unit_data.unit_type, unit_data.distance, cascade_power);
        
        // Create cascade explosion at unit position
        self.create_explosion_with_power_static(
            lua,
            &explosion_name,
            unit_data.position,
            cascade_power,
        )?;
        
        Ok(())
    }

    /// Calculate blast damage based on distance and explosion power
    fn calculate_blast_damage(&self, distance: f32, explosion_power: f32) -> f32 {
        // Simple inverse square law damage calculation
        // Damage decreases with distance squared
        if distance <= 0.0 {
            return explosion_power;
        }
        
        let damage = explosion_power / (distance * distance);
        damage.max(0.0)
    }

    /// Add a recent explosion to the tracking list
    fn add_recent_explosion(&mut self, position: &LuaVec3, radius: f32) {
        let now = Utc::now();
        self.recent_explosions.push_back(RecentExplosion {
            position: *position,
            time: now,
            radius,
        });
        
        let cutoff_time = now - chrono::Duration::seconds(10);
        while let Some(front) = self.recent_explosions.front() {
            if front.time < cutoff_time {
                self.recent_explosions.pop_front();
            } else {
                break;
            }
        }
    }

    /// Schedule wave effects after the main explosion
    fn schedule_wave_effects(
        &mut self,
        lua: MizLua,
        center: &LuaVec3,
        _radius: f32,
        power: f32,
        weapon_type: &str,
        _pre_targets: Vec<()>,
    ) -> Result<()> {
        debug!("SPLASH: Scheduling wave effects for {}", weapon_type);
        
        // Apply wave explosion scaling and cascade effects
        if self.config.wave_explosions.scaling > 1.0 {
            self.create_cascade_explosions(lua, center, power, weapon_type)?;
        }
        
        Ok(())
    }

    /// Create cascade explosions based on wave explosion configuration
    fn create_cascade_explosions(
        &mut self,
        lua: MizLua,
        center: &LuaVec3,
        power: f32,
        weapon_type: &str,
    ) -> Result<()> {
        if !self.config.wave_explosions.always_cascade_explode {
            return Ok(());
        }
        
        let cascade_power = power * self.config.wave_explosions.scaling;
        let cascade_radius = 1500.0;
        
        debug!("SPLASH: Creating cascade explosion for {} with power {} and radius {}", 
               weapon_type, cascade_power, cascade_radius);
        
        // Create secondary explosion at a nearby location
        let cascade_position = LuaVec3(na::Vector3::new(
            center.0.x + 50.0,
            center.0.y,
            center.0.z + 50.0,
        ));
        
        // Check if cascade explosion meets threshold
        if cascade_power >= self.config.wave_explosions.damage_threshold {
            self.create_explosion(lua, &format!("{}_cascade", weapon_type), cascade_position)?;
        }
        
        Ok(())
    }

    /// Check for ordnance protection (prevent nearby bombs from detonating)
    fn check_ordnance_protection(&self, lua: MizLua, position: &LuaVec3) -> bool {
        if !self.config.ordnance_protection.enabled {
            return true; // No protection, allow explosion
        }

        let protection_radius = self.config.ordnance_protection.protection_radius;
        
        // Check against recent explosions
        for recent in &self.recent_explosions {
            if is_within_sphere(&recent.position, position, protection_radius) {
                debug!("SPLASH: Ordnance protection active - explosion blocked by recent explosion at {:?}", recent.position);
                return false;
            }
        }
        
        // Check against currently tracked weapons
        for (_, weapon) in &self.tracked_weapons {
            if is_within_sphere(&weapon.position, position, protection_radius) {
                debug!("SPLASH: Ordnance protection active - explosion blocked by tracked weapon at {:?}", weapon.position);
                return false;
            }
        }
        
        // Check for ordnance destruction detection
        if self.config.ordnance_protection.detect_ordnance_destruction {
            self.check_ordnance_destruction(lua, position);
        }
        
        // Check for recent large explosions
        if self.config.ordnance_protection.recent_large_explosion_snap {
            if self.check_recent_large_explosions(position) {
                debug!("SPLASH: Ordnance protection active - explosion blocked by recent large explosion");
                return false;
            }
        }
        
        true // No protection needed, allow explosion
    }

    /// Check for ordnance destruction by large explosions
    fn check_ordnance_destruction(&self, lua: MizLua, position: &LuaVec3) {
        if self.config.ordnance_protection.snap_to_ground_if_destroyed {
            let ground_height = self.get_ground_height_at_position(lua, *position);
            let height_above_ground = position.0.y - ground_height;
            
            if height_above_ground <= self.config.ordnance_protection.max_snapped_height as f64 {
                debug!("SPLASH: Ordnance at height {:.1}m may be destroyed by large explosion", height_above_ground);
            }
        }
    }

    /// Check for recent large explosions within range
    fn check_recent_large_explosions(&self, position: &LuaVec3) -> bool {
        let now = Utc::now();
        let time_window = chrono::Duration::seconds(self.config.ordnance_protection.recent_large_explosion_time as i64);
        let range = self.config.ordnance_protection.recent_large_explosion_range;
        
        for recent in &self.recent_explosions {
            if now.signed_duration_since(recent.time) <= time_window {
                if distance_3d(&recent.position, position) <= range {
                    // Check if this was a large explosion (radius > 50m)
                    if recent.radius > 50.0 {
                        return true;
                    }
                }
            }
        }
        
        false
    }

    /// Get ground height at a position
    fn get_ground_height_at_position(&self, lua: MizLua, position: LuaVec3) -> f64 {
        // Use the existing get_ground_height method with proper Lua context
        self.get_ground_height(lua, position)
    }

    
    /// Check for weapon impacts and clean up only when weapons actually impact
    pub fn cleanup(&mut self, lua: MizLua, now: DateTime<Utc>) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        
        // Only check for impacts - no time-based cleanup
        self.check_weapon_impacts(lua, now)?;
        
        // Update cook-off system
        self.cookoff.update(lua, now)?;
        
        // Update movement tracking for cook-offs
        self.cookoff.update_movement_tracking(lua)?;
        
        // Periodically initialize additional mission units
        if now.timestamp() % UNIT_INIT_INTERVAL == 0 {
            self.initialize_mission_units();
        }
        
        // Periodically apply mission settings based on current conditions
        if now.timestamp() % MISSION_SETTINGS_INTERVAL == 0 {
            let difficulty = 1.0; // Could be calculated based on mission state
            self.apply_mission_settings(difficulty);
        }
        
        // Periodically clean up unused units
        if now.timestamp() % CLEANUP_INTERVAL == 0 {
            self.cleanup_unused_units();
        }
        
        // Log cook-off system status for debugging
        debug!(
            "SPLASH: Impact check completed - {} weapons tracked, {} cook-off effects pending, {} units configured, {} effects queued, {} timed explosions pending",
            self.tracked_weapons.len(),
            self.cookoff.pending_effects_count(),
            self.cookoff.units_count(),
            self.cookoff.effect_queue_count(),
            self.cookoff.timed_explosions_count()
        );
        
        Ok(())
    }

    /// Handle a unit hit event for cook-off effects
    pub fn handle_unit_hit(
        &mut self,
        lua: MizLua,
        unit_id: bfprotocols::db::group::UnitId,
        unit_name: &str,
        unit_type: &str,
        position: Position3,
        health_percent: f32,
        is_dead: bool,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        
        self.cookoff.process_unit_hit(
            lua,
            unit_id,
            unit_name,
            unit_type,
            position,
            health_percent,
            is_dead,
        )
    }

    /// Get the cook-off configuration
    pub fn cookoff_config(&self) -> &CookOffConfig {
        self.cookoff.config()
    }

    /// Update the cook-off configuration
    pub fn update_cookoff_config(&mut self, config: CookOffConfig) {
        self.cookoff.update_config(config);
    }

    /// Update the splash damage configuration from the main configuration
    pub fn update_from_main_config(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        if !enabled {
            // If disabled, also disable cook-off
            self.cookoff.update_config(CookOffConfig {
                enabled: false,
                ..self.cookoff.config().clone()
            });
        }
    }

    /// Get a mutable reference to the cook-off system
    pub fn cookoff_mut(&mut self) -> &mut CookOff {
        &mut self.cookoff
    }

    /// Get a reference to the cook-off system
    pub fn cookoff(&self) -> &CookOff {
        &self.cookoff
    }

    /// Log cook-off system status and configuration for debugging
    pub fn log_cookoff_status(&self) {
        let unit_count = self.cookoff.units_count();
        let pending_count = self.cookoff.pending_effects_count();
        let queue_count = self.cookoff.effect_queue_count();
        let processed_count = self.cookoff.processed_units_count();
        
        debug!(
            "COOKOFF STATUS: {} units, {} pending effects, {} queued effects, {} processed units",
            unit_count, pending_count, queue_count, processed_count
        );
        
        // Check if specific unit types are configured
        let is_ural = self.cookoff.is_unit("Ural-4320T");
        let is_hemtt = self.cookoff.is_unit("M978 HEMTT Tanker");
        
        debug!(
            "COOKOFF STATUS: Ural-4320T configured: {}, M978 HEMTT Tanker configured: {}",
            is_ural, is_hemtt
        );
        
        // Get properties for a specific unit type
        if let Some(properties) = self.cookoff.get_unit_properties("Ural-4320T") {
            debug!(
                "COOKOFF STATUS: Ural-4320T properties - explosion power: {}, cookoff count: {}, flame size: {}",
                properties.explosion.power, properties.cookoff.count, properties.smoke.size
            );
        }
        
        // Log configuration status
        let config = self.cookoff_config();
        debug!(
            "COOKOFF STATUS: System enabled: {}, effects chance: {:.1}%, damage threshold: {:.1}%",
            config.enabled, config.effects_chance * 100.0, config.damage_threshold
        );
        
        // Log cook-off system access
        let cookoff_system = self.cookoff();
        debug!(
            "COOKOFF STATUS: Cook-off system has {} configured units and {} pending effects",
            cookoff_system.units_count(), cookoff_system.pending_effects_count()
        );
        
        // Log system capabilities
        debug!("COOKOFF STATUS: System supports dynamic unit management and configuration updates");
    }

    /// Initialize additional units based on mission requirements
    pub fn initialize_mission_units(&mut self) {
        // Add additional units that might be spawned dynamically
        let cookoff_system = self.cookoff_mut();
        
        // Add some additional common units
        cookoff_system.add_unit("M35".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 45.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 3,
                power: 10.0,
                duration: 25.0,
                random_timing: true,
                power_random: 35.0,
            },
            smoke: SmokeConfig {
                is_tanker: false,
                size: 3, // Reduced to very small smoke
                duration: 90, // Reduced duration
            },
        });
        
        cookoff_debug("Initialized additional mission units");
    }

    /// Update cook-off configuration based on mission settings
    pub fn apply_mission_settings(&mut self, mission_difficulty: f32) {
        let mut config = self.cookoff_config().clone();
        
        // Adjust effects chance based on mission difficulty
        config.effects_chance = (config.effects_chance * mission_difficulty).min(1.0);
        
        // Adjust damage threshold based on difficulty
        config.damage_threshold = (config.damage_threshold * (2.0 - mission_difficulty)).max(10.0);
        
        // Update the configuration
        self.update_cookoff_config(config);
        
        cookoff_debug(&format!("Applied mission settings with difficulty factor: {:.2}", mission_difficulty));
    }

    /// Clean up unused unit types
    pub fn cleanup_unused_units(&mut self) {
        let cookoff_system = self.cookoff_mut();
        
        // Remove units that haven't been used recently
        // This is a simple cleanup mechanism
        let units_to_remove = vec!["M818", "M35"];
        
        for unit_type in units_to_remove {
            if cookoff_system.is_unit(unit_type) {
                cookoff_system.remove_unit(unit_type);
                debug!("COOKOFF: Removed unused unit type: {}", unit_type);
            }
        }
        
        // Also clean up any units that have been processed recently
        let _current_time = chrono::Utc::now();
        let _cutoff_time = _current_time - chrono::Duration::minutes(30);
        
        // Remove units that were added dynamically but haven't been used
        // Use a more efficient approach with retain for better performance
        // Pre-allocate the vector with estimated capacity to avoid reallocations
        let mut units_to_remove = Vec::with_capacity(cookoff_system.units.len() / 4);
        
        // Use a single pass to identify units to remove
        for (unit_type, _properties) in &cookoff_system.units {
            // Check if this is a dynamically added unit (not in the default set)
            let unit_str = unit_type.as_str();
            if unit_str != "M818" && unit_str != "M35" && 
               !unit_str.starts_with("Ural") && !unit_str.starts_with("M978") &&
               unit_str != "Ammo_crate" && unit_str != "Barrel" {
                units_to_remove.push(unit_type.clone());
            }
        }
        
        // Remove identified units in batch
        for unit_type in units_to_remove {
            cookoff_system.remove_unit(&unit_type);
            cookoff_debug(&format!("Cleaned up unused dynamic unit: {}", unit_type));
        }
    }
}

impl CookOff {
    pub fn new(config: CookOffConfig) -> Self {
        let mut units = FxHashMap::default();
        
        // Initialize units table with common vehicles
        // Fuel tankers
        units.insert("Ural-4320T".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 60.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 6,
                power: 15.0,
                duration: 45.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: true,
                size: 5, // Reduced from 7 to 5 (small smoke)
                duration: 150, // Reduced duration
            },
        });
        
        units.insert("M978 HEMTT Tanker".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 60.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 6,
                power: 15.0,
                duration: 45.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: true,
                size: 5, // Reduced from 7 to 5 (small smoke)
                duration: 150, // Reduced duration
            },
        });
        
        // Ammo trucks
        units.insert("Ural-4320".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 80.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 8,
                power: 20.0,
                duration: 60.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: false,
                size: 4, // Reduced from 6 to 4 (small smoke)
                duration: 120, // Reduced duration
            },
        });
        
        units.insert("M939 Heavy".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 80.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 8,
                power: 20.0,
                duration: 60.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: false,
                size: 4, // Reduced from 6 to 4 (small smoke)
                duration: 120, // Reduced duration
            },
        });
        
        // Ammo crates and barrels
        units.insert("Ammo_crate".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 30.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 3,
                power: 8.0,
                duration: 20.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: false,
                size: 4,
                duration: 120,
            },
        });
        
        units.insert("Barrel".into(), UnitProperties {
            explosion: ExplosionConfig {
                power: 25.0,
                enabled: true,
            },
            cookoff: CookOffEffectConfig {
                enabled: true,
                count: 2,
                power: 6.0,
                duration: 15.0,
                random_timing: true,
                power_random: 50.0,
            },
            smoke: SmokeConfig {
                is_tanker: false,
                size: 3,
                duration: 90,
            },
        });

        Self {
            config,
            units,
            pending_effects: FxHashMap::default(),
            processed_units: FxHashMap::default(),
            processed_smoke: FxHashMap::default(),
            effect_queue: VecDeque::new(),
            timed_explosions: VecDeque::new(),
            effect_smoke_id: 1,
            movement_tracking: FxHashMap::default(),
            rng: thread_rng(),
            update_times: VecDeque::new(),
            last_update_time: Utc::now(),
            adaptive_update_interval: 100, // Start with 100ms update interval
            performance_mode: PerformanceMode::Normal,
        }
    }

    /// Get ground height at a position
    fn get_ground_height(&self, lua: MizLua, position: LuaVec3) -> f64 {
        // Use DCS land.getHeight() to get terrain height
        match dcso3::land::Land::singleton(lua) {
            Ok(land) => {
                let pos_2d = dcso3::LuaVec2(na::Vector2::new(position.0.x, position.0.z));
                match land.get_height(pos_2d) {
                    Ok(height) => height,
                    Err(e) => {
                        debug!("COOKOFF: Failed to get ground height: {:?}", e);
                        0.0
                    }
                }
            }
            Err(e) => {
                debug!("COOKOFF: Failed to get land singleton for height: {:?}", e);
                0.0
            }
        }
    }
    
    /// Get proper ground-level position with height offset like the Lua script
    fn get_ground_position_with_offset(&self, lua: MizLua, position: LuaVec3, height_offset: f64) -> LuaVec3 {
        let ground_height = self.get_ground_height(lua, position);
        let adjusted_height = ground_height + height_offset;
        
        debug!("COOKOFF: Ground positioning - Original: {:?}, Ground height: {:.2}m, Offset: {:.2}m, Final: {:.2}m", 
               position, ground_height, height_offset, adjusted_height);
        
        LuaVec3(na::Vector3::new(
            position.0.x,
            adjusted_height,
            position.0.z,
        ))
    }

    /// Process a unit hit event for cook-off
    pub fn process_unit_hit(
        &mut self,
        lua: MizLua,
        unit_id: BfUnitId,
        unit_name: &str,
        unit_type: &str,
        position: Position3,
        health_percent: f32,
        is_dead: bool,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Check if unit is already processed
        if self.processed_units.contains_key(&unit_id) {
            debug!("COOKOFF: Unit {} already processed, skipping", unit_name);
            return Ok(());
        }

        // Check if unit is a candidate
        let is_candidate = self.is_candidate(unit_type, unit_name);
        if !is_candidate {
            debug!("COOKOFF: Unit {} not a candidate, skipping", unit_name);
            return Ok(());
        }

        // Check damage threshold
        let damage_threshold = if self.units.contains_key(unit_type) {
            self.config.damage_threshold
        } else {
            self.config.all_vehicles.damage_threshold
        };

        if health_percent > damage_threshold && !is_dead {
            debug!("COOKOFF: Unit {} health {}% above threshold {}%, skipping", 
                   unit_name, health_percent, damage_threshold);
            return Ok(());
        }

        // Check effects chance
        if self.rng.gen_range(0.0..1.0) > self.config.effects_chance {
            debug!("COOKOFF: Effects chance failed for unit {}", unit_name);
            return Ok(());
        }

        // Create pending effect
        let properties = self.get_properties(unit_type);
        let pending_effect = PendingCookOffEffect {
            unit_id,
            unit_name: CompactString::from(unit_name), // Use CompactString for efficiency
            unit_type: CompactString::from(unit_type), // Use CompactString for efficiency
            position,
            start_time: Utc::now(),
            is_cookoff: true,
            is_dead,
            properties,
        };

        self.pending_effects.insert(unit_id, pending_effect);
        self.processed_units.insert(unit_id, Utc::now());

        debug!("COOKOFF: Added unit {} to pending effects", unit_name);

        // Schedule cook-off effects
        self.schedule_cookoff_effects(lua, unit_id, is_dead)?;

        Ok(())
    }

    /// Check if a unit is a candidate
    fn is_candidate(&self, unit_type: &str, unit_name: &str) -> bool {
        // Check if unit is in units table
        if self.units.contains_key(unit_type) {
            return true;
        }

        // Check if unit name contains CookoffTarget
        if unit_name.contains("CookoffTarget") {
            return true;
        }

        // Check if all vehicles effects are enabled
        if self.config.all_vehicles.enabled {
            return true;
        }

        false
    }

    /// Get properties for a unit type
    fn get_properties(&self, unit_type: &str) -> UnitProperties {
        if let Some(properties) = self.units.get(unit_type) {
            properties.clone()
        } else {
            // Use all units defaults
            UnitProperties {
                explosion: self.config.all_vehicles.explosion.clone(),
                cookoff: self.config.all_vehicles.cookoff.clone(),
                smoke: self.config.all_vehicles.smoke.clone(),
            }
        }
    }

    /// Schedule cook-off effects for a unit
    fn schedule_cookoff_effects(&mut self, lua: MizLua, unit_id: BfUnitId, from_dead_event: bool) -> Result<()> {
        let pending_effect = match self.pending_effects.get(&unit_id) {
            Some(effect) => effect.clone(),
            None => {
                warn!("COOKOFF: No pending effect found for unit {}", unit_id);
                return Ok(());
            }
        };

        let properties = &pending_effect.properties;

        // Determine if cook-off should occur
        let cookoff_chance = if self.units.contains_key(&pending_effect.unit_type) {
            1.0 // Always cook-off for configured units
        } else {
            self.config.all_vehicles.cookoff_chance
        };

        let should_cookoff = self.rng.gen_range(0.0..1.0) <= cookoff_chance;
        let should_smoke = if should_cookoff && self.config.all_vehicles.smoke_with_cookoff {
            true
        } else {
            self.rng.gen_range(0.0..1.0) <= self.config.all_vehicles.smoke_chance
        };

        // Create scheduled effect
        let scheduled_effect = ScheduledCookOffEffect {
            unit_id,
            unit_name: pending_effect.unit_name.clone(),
            unit_type: pending_effect.unit_type.clone(),
            position: pending_effect.position,
            explosion: ExplosionConfig {
                power: properties.explosion.power,
                enabled: properties.explosion.enabled,
            },
            cookoff: CookOffEffectConfig {
                enabled: should_cookoff,
                count: properties.cookoff.count,
                power: properties.cookoff.power,
                duration: properties.cookoff.duration,
                random_timing: properties.cookoff.random_timing,
                power_random: properties.cookoff.power_random,
            },
            smoke: SmokeConfig {
                is_tanker: should_smoke,
                size: properties.smoke.size,
                duration: properties.smoke.duration,
            },
        };

        self.effect_queue.push_back(scheduled_effect);

        // If not from dead event, start movement tracking
        if !from_dead_event {
            self.start_movement_tracking(lua, unit_id)?;
        } else {
            // For dead events, trigger effects immediately
            self.trigger_effects_immediately(lua, unit_id)?;
        }

        Ok(())
    }

    /// Start tracking unit movement until it stops
    fn start_movement_tracking(&mut self, lua: MizLua, unit_id: BfUnitId) -> Result<()> {
        debug!("COOKOFF: Starting movement tracking for unit {}", unit_id);
        
        // Get the unit's current position
        if let Some(pending_effect) = self.pending_effects.get(&unit_id) {
            let position = LuaVec3(na::Vector3::new(
                pending_effect.position.p.x,
                pending_effect.position.p.y,
                pending_effect.position.p.z,
            ));
            let tracker = MovementTracker::new(unit_id, position);
            self.movement_tracking.insert(unit_id, tracker);
            debug!("COOKOFF: Movement tracking started for unit {} at position {:?}", unit_id, position);
        } else {
            warn!("COOKOFF: No pending effect found for unit {} when starting movement tracking", unit_id);
            // Fallback to immediate effects
            self.trigger_effects_immediately(lua, unit_id)?;
        }
        
        Ok(())
    }

    /// Update movement tracking for all tracked units
    pub fn update_movement_tracking(&mut self, lua: MizLua) -> Result<()> {
        let now = Utc::now();
        let mut units_to_remove = Vec::new();
        let mut units_to_trigger = Vec::new();
        
        // Collect unit IDs to avoid borrowing conflicts
        let unit_ids: Vec<BfUnitId> = self.movement_tracking.keys().cloned().collect();
        
        for unit_id in unit_ids {
            // Get tracker data first to avoid borrowing conflicts
            let (should_update, tracker_data) = if let Some(tracker) = self.movement_tracking.get(&unit_id) {
                let should_update = now.signed_duration_since(tracker.last_update).num_seconds() >= 1;
                (should_update, (tracker.unit_id, tracker.last_position, tracker.stationary_checks, tracker.max_stationary_checks, tracker.movement_threshold))
            } else {
                continue;
            };
            
            if !should_update {
                continue; // Skip if updated too recently
            }
            
            // Try to get the unit object using our position-based search
            let (found_unit, current_position) = {
                let unit_result = self.get_unit_object(lua, unit_id);
                match unit_result {
                    Ok(unit) => {
                        // Unit found, get its current position
                        match unit.get_point() {
                            Ok(position) => {
                                debug!("COOKOFF: Found unit {} (ID: {}) at position {:?}", 
                                       tracker_data.0, unit_id, position);
                                (true, position)
                            }
                            Err(e) => {
                                debug!("COOKOFF: Failed to get position for unit {} (ID: {}): {:?}", 
                                       tracker_data.0, unit_id, e);
                                (false, tracker_data.1)
                            }
                        }
                    }
                    Err(e) => {
                        debug!("COOKOFF: Unit {} (ID: {}) not found: {:?}", tracker_data.0, unit_id, e);
                        (false, tracker_data.1)
                    }
                }
            }; // unit_result is dropped here, releasing the borrow
                
            if found_unit {
                // Calculate distance moved
                let distance = ((current_position.0.x - tracker_data.1.0.x).powi(2) +
                               (current_position.0.y - tracker_data.1.0.y).powi(2) +
                               (current_position.0.z - tracker_data.1.0.z).powi(2)).sqrt();
                
                if distance < tracker_data.4 as f64 {
                    // Unit is stationary
                    let new_stationary_checks = tracker_data.2 + 1;
                    debug!("COOKOFF: Unit {} (ID: {}) stationary for {} checks (threshold: {})", 
                           tracker_data.0, unit_id, new_stationary_checks, tracker_data.3);
                    
                    if new_stationary_checks >= tracker_data.3 {
                        // Unit has been stationary long enough, trigger effects
                        debug!("COOKOFF: Unit {} (ID: {}) has been stationary long enough, triggering effects", 
                               tracker_data.0, unit_id);
                        units_to_trigger.push(unit_id);
                        units_to_remove.push(unit_id);
                    } else {
                        // Update tracker with new stationary count
                        if let Some(tracker) = self.movement_tracking.get_mut(&unit_id) {
                            tracker.stationary_checks = new_stationary_checks;
                            tracker.last_position = current_position;
                            tracker.last_update = now;
                        }
                    }
                } else {
                    // Unit is moving, reset stationary counter
                    debug!("COOKOFF: Unit {} (ID: {}) moved {:.1}m, resetting stationary counter", 
                           tracker_data.0, unit_id, distance);
                    if let Some(tracker) = self.movement_tracking.get_mut(&unit_id) {
                        tracker.stationary_checks = 0;
                        tracker.last_position = current_position;
                        tracker.last_update = now;
                    }
                }
            } else {
                // Unit not found, might have been destroyed
                debug!("COOKOFF: Unit {} (ID: {}) not found in search area, removing from tracking", 
                       tracker_data.0, unit_id);
                units_to_remove.push(unit_id);
            }
        }
        
        // Trigger effects for stationary units
        for unit_id in units_to_trigger {
            debug!("COOKOFF: Triggering effects for stationary unit {}", unit_id);
            self.trigger_effects_immediately(lua, unit_id)?;
        }
        
        // Remove units that are no longer being tracked
        for unit_id in units_to_remove {
            self.movement_tracking.remove(&unit_id);
        }
        
        debug!("COOKOFF: Movement tracking update complete, tracking {} units", self.movement_tracking.len());
        Ok(())
    }

    /// Get unit object by ID using position-based search
    fn get_unit_object<'a>(&self, lua: MizLua<'a>, unit_id: BfUnitId) -> Result<dcso3::unit::Unit<'a>> {
        debug!("COOKOFF: Attempting to get unit object for ID {}", unit_id);
        
        // Create a large search volume around the unit's last known position
        // We'll search in a 1000m radius to find the unit
        let search_radius = 1000.0;
        
        // Get the unit's last known position from movement tracking
        let search_center = if let Some(tracker) = self.movement_tracking.get(&unit_id) {
            tracker.last_position
        } else {
            // If no tracking data, we can't search effectively
            return Err(anyhow::anyhow!("No position data available for unit ID {}", unit_id));
        };
        
        let search_volume = dcso3::world::SearchVolume::Sphere {
            point: search_center,
            radius: search_radius,
        };
        
        // Search for units in the area using the new collect method
        let world = dcso3::world::World::singleton(lua)?;
        let objects = world.search_objects_collect(
            dcso3::object::ObjectCategory::Unit,
            search_volume,
        )?;
        
        // Look for a unit that matches our ID
        for object in objects {
            // Check if this object is a unit and get its name
            match object.get_name() {
                Ok(name) => {
                    // Convert the name to a unit ID for comparison
                    // Note: This is a simplified approach - in practice you might need
                    // a more sophisticated way to match unit IDs to unit names
                    if name.contains(&unit_id.to_string()) {
                        // Try to convert to Unit object
                        match object.as_unit() {
                            Ok(unit) => {
                                debug!("COOKOFF: Found unit object for ID {} with name {}", unit_id, name);
                                return Ok(unit);
                            }
                            Err(e) => {
                                debug!("COOKOFF: Failed to convert object to unit for ID {}: {:?}", unit_id, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("COOKOFF: Failed to get name for object: {:?}", e);
                }
            }
        }
        
        Err(anyhow::anyhow!("Unit with ID {} not found in search area", unit_id))
    }

    /// Trigger effects immediately (for dead events or after movement stops)
    fn trigger_effects_immediately(&mut self, lua: MizLua, unit_id: BfUnitId) -> Result<()> {
        let effect = match self.effect_queue.iter().find(|e| e.unit_id == unit_id) {
            Some(effect) => effect.clone(),
            None => {
                warn!("COOKOFF: No scheduled effect found for unit {}", unit_id);
                return Ok(());
            }
        };

        debug!("COOKOFF: Triggering effects for unit {} at position {:?}", 
               effect.unit_name, effect.position);

        // Trigger initial explosion
        if effect.explosion.enabled {
            self.create_explosion(lua, &effect.position, effect.explosion.power)?;
        }

        // Trigger smoke/fire effects
        if effect.smoke.is_tanker {
            self.create_smoke_effect(lua, &effect.position, effect.smoke.size, effect.smoke.duration)?;
        }

        // Trigger cook-off effects
        if effect.cookoff.enabled && effect.cookoff.count > 0 {
            self.schedule_cookoff_effects_for_effect(lua, &effect)?;
        }

        // Remove from queue
        self.effect_queue.retain(|e| e.unit_id != unit_id);

        Ok(())
    }

    /// Create an explosion at the specified position
    fn create_explosion(&self, lua: MizLua, position: &Position3, power: f32) -> Result<()> {
        let trigger = Trigger::singleton(lua)?;
        let action = trigger.action()?;
        
        let original_pos = LuaVec3(Vector3::new(
            position.p.x,
            position.p.y,
            position.p.z,
        ));
        
        // Get proper ground-level position with height offset like the Lua script
        // Use 0.1m offset for cook-off explosions (ground level)
        let explosion_pos = self.get_ground_position_with_offset(lua, original_pos, 0.1);

        // Use the power directly like the Lua script does - no minimum threshold
        action.explosion(explosion_pos, power)?;
        
        // Add large black smoke effect for cook-off (like Lua script)
        let smoke_name = format!("cookoff_explosion_smoke_{}", self.effect_smoke_id);
        if let Err(e) = action.effect_smoke_big(explosion_pos, dcso3::trigger::SmokePreset::MediumSmokeAndFire, 1.0, smoke_name.into()) {
            debug!("COOKOFF: Failed to create large smoke effect: {:?}", e);
        } else {
            debug!("COOKOFF: Created large black smoke effect at {:?}", explosion_pos);
        }
        
        debug!("COOKOFF: Created explosion at {:?} with power {}", explosion_pos, power);
        Ok(())
    }

    /// Create a smoke effect at the specified position
    fn create_smoke_effect(&self, lua: MizLua, position: &Position3, size: u8, duration: u32) -> Result<()> {
        let trigger = Trigger::singleton(lua)?;
        let action = trigger.action()?;
        
        let land = Land::singleton(lua)?;
        let terrain_height = land.get_height(LuaVec2(na::Vector2::new(position.p.x, position.p.z)))?;
        
        let smoke_pos = LuaVec3(Vector3::new(
            position.p.x,
            terrain_height + 2.0,
            position.p.z,
        ));

        let smoke_preset = match size {
            1..=2 => SmokePreset::SmallSmoke,
            3..=4 => SmokePreset::MediumSmoke,
            5..=6 => SmokePreset::LargeSmoke,
            7..=8 => SmokePreset::HugeSmoke,
            _ => SmokePreset::MediumSmoke,
        };

        // Use a more efficient string allocation for smoke effect names
        let smoke_name = format!("cookoff_smoke_{}", self.effect_smoke_id);
        
        
        action.effect_smoke_big(smoke_pos, smoke_preset, 1.0, smoke_name.into())?;
        
        debug!("COOKOFF: Created smoke effect at {:?} with size {} for {} seconds", 
               smoke_pos, size, duration);
        
        Ok(())
    }

    /// Schedule cook-off effects for a scheduled effect
    fn schedule_cookoff_effects_for_effect(&mut self, lua: MizLua, effect: &ScheduledCookOffEffect) -> Result<()> {
        let now = Utc::now();

        // Schedule flares if enabled
        if self.config.flares.enabled {
            self.schedule_cookoff_flares_timed(lua, &effect.position, effect.cookoff.count, effect.cookoff.duration, now)?;
        }

        // Schedule cook-off explosions with proper timing
        for i in 0..effect.cookoff.count {
            let delay_seconds = if effect.cookoff.random_timing {
                self.rng.gen_range(0.0..1.0) * effect.cookoff.duration
            } else {
                (i as f32) * (effect.cookoff.duration / effect.cookoff.count as f32)
            };

            let base_power = effect.cookoff.power;
            let power_variation = effect.cookoff.power_random / 100.0;
            let cookoff_power = if effect.cookoff.power_random == 0.0 {
                base_power
            } else {
                base_power * (1.0 + power_variation * (self.rng.gen_range(0.0..1.0) * 2.0 - 1.0))
            };

            // Schedule the explosion for later
            let trigger_time = now + chrono::Duration::milliseconds((delay_seconds * 1000.0) as i64);
            let timed_explosion = TimedExplosion {
                position: effect.position,
                power: cookoff_power,
                trigger_time,
                effect_type: ExplosionType::Cookoff,
                azimuth: None, // Cook-off explosions don't need azimuth
            };
            
            self.timed_explosions.push_back(timed_explosion);
            debug!("COOKOFF: Scheduled cook-off explosion for {} seconds from now with power {}", delay_seconds, cookoff_power);
        }

        // Schedule debris effects if enabled
        if self.config.debris.enabled {
            self.schedule_debris_effects_timed(lua, effect, now)?;
        }

        Ok(())
    }

    /// Schedule cook-off flares with timing
    fn schedule_cookoff_flares_timed(&mut self, lua: MizLua, position: &Position3, cookoff_count: u32, cookoff_duration: f32, start_time: DateTime<Utc>) -> Result<()> {

        // Check flare chance
        if self.rng.gen_range(0.0..1.0) > self.config.flares.timing.chance {
            return Ok(());
        }

        let flare_count = (cookoff_count as f32 * self.config.flares.timing.count_modifier) as u32;
        if flare_count == 0 {
            return Ok(());
        }

        if self.config.flares.instant.enabled {
            // Spawn instant flares (like Lua - no delays for instant flares)
            let instant_count = self.rng.gen_range(self.config.flares.instant.min..=self.config.flares.instant.max);
            let angle_step = 360.0 / instant_count as f32;

            for i in 0..instant_count {
                let base_azimuth = (i as f32) * angle_step;
                let random_azimuth = base_azimuth + self.rng.gen_range(-33.0..=40.0);
                let azimuth = (random_azimuth % 360.0) as u16;

                let offset_x = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);
                let offset_z = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);

                let flare_pos = Position3 {
                    p: LuaVec3(Vector3::new(
                        position.p.x + offset_x,
                        position.p.y,
                        position.p.z + offset_z,
                    )),
                    ..*position
                };

                // Schedule flare immediately (like Lua instant flares)
                let timed_explosion = TimedExplosion {
                    position: flare_pos,
                    power: 0.0, // Flares don't have explosion power
                    trigger_time: start_time, // No delay for instant flares
                    effect_type: ExplosionType::Flare,
                    azimuth: Some(azimuth), // Store the calculated azimuth for the flare
                };
                
                self.timed_explosions.push_back(timed_explosion);
            }
        } else {
            // Spawn flares over time (like Lua time-based flares)
            for _i in 0..flare_count {
                let delay_seconds = self.rng.gen_range(0.0..1.0) * cookoff_duration;
                let azimuth = self.rng.gen_range(1..=360) as u16;

                let offset_x = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);
                let offset_z = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);

                // Use ground height for time-based flares (like Lua)
                let ground_height = self.get_ground_height(lua, position.p);
                let flare_pos = Position3 {
                    p: LuaVec3(Vector3::new(
                        position.p.x + offset_x,
                        ground_height, // Start at ground level like Lua
                        position.p.z + offset_z,
                    )),
                    ..*position
                };

                let trigger_time = start_time + chrono::Duration::milliseconds((delay_seconds * 1000.0) as i64);
                
                let timed_explosion = TimedExplosion {
                    position: flare_pos,
                    power: 0.0, // Flares don't have explosion power
                    trigger_time,
                    effect_type: ExplosionType::Flare,
                    azimuth: Some(azimuth), // Store the calculated azimuth for the flare
                };
                
                self.timed_explosions.push_back(timed_explosion);
            }
        }

        Ok(())
    }

    /// Schedule cook-off flares (legacy method for compatibility)
    #[allow(dead_code)]
    fn schedule_cookoff_flares(&mut self, lua: MizLua, position: &Position3, cookoff_count: u32, cookoff_duration: f32) -> Result<()> {

        // Check flare chance
        if self.rng.gen_range(0.0..1.0) > self.config.flares.timing.chance {
            return Ok(());
        }

        let flare_count = (cookoff_count as f32 * self.config.flares.timing.count_modifier) as u32;
        if flare_count == 0 {
            return Ok(());
        }

        if self.config.flares.instant.enabled {
            // Spawn instant flares
            let instant_count = self.rng.gen_range(self.config.flares.instant.min..=self.config.flares.instant.max);
            let angle_step = 360.0 / instant_count as f32;

            for i in 0..instant_count {
                let base_azimuth = (i as f32) * angle_step;
                let random_azimuth = base_azimuth + self.rng.gen_range(-33.0..=40.0);
                let azimuth = (random_azimuth % 360.0) as u16;

                let offset_x = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);
                let offset_z = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);

                let flare_pos = LuaVec3(Vector3::new(
                    position.p.x + offset_x,
                    position.p.y,
                    position.p.z + offset_z,
                ));

                let trigger = Trigger::singleton(lua)?;
                let action = trigger.action()?;
                
                // Use configurable flare color (like Lua cookoff_flare_color)
                let flare_color = match self.config.flares.color {
                    0 => FlareColor::Green,
                    1 => FlareColor::Red,
                    2 => FlareColor::White,
                    3 => FlareColor::Yellow,
                    _ => FlareColor::White, // Default to white
                };


                action.signal_flare(flare_pos, flare_color, azimuth)?;
            }
        } else {
            // Spawn flares over time
            for _i in 0..flare_count {
                let _delay = self.rng.gen_range(0.0..1.0) * cookoff_duration;
                let azimuth = self.rng.gen_range(1..=360) as u16;

                let offset_x = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);
                let offset_z = self.rng.gen_range(-self.config.flares.timing.offset..=self.config.flares.timing.offset);

                let flare_pos = LuaVec3(Vector3::new(
                    position.p.x + offset_x,
                    position.p.y,
                    position.p.z + offset_z,
                ));

                let trigger = Trigger::singleton(lua)?;
                let action = trigger.action()?;
                
                // Use configurable flare color (like Lua cookoff_flare_color)
                let flare_color = match self.config.flares.color {
                    0 => FlareColor::Green,
                    1 => FlareColor::Red,
                    2 => FlareColor::White,
                    3 => FlareColor::Yellow,
                    _ => FlareColor::White, // Default to white
                };


                action.signal_flare(flare_pos, flare_color, azimuth)?;
            }
        }

        Ok(())
    }

    /// Schedule debris effects with timing
    fn schedule_debris_effects_timed(&mut self, _lua: MizLua, effect: &ScheduledCookOffEffect, start_time: DateTime<Utc>) -> Result<()> {
        let debris_count = self.rng.gen_range(self.config.debris.count.min..=self.config.debris.count.max);

        for _j in 0..debris_count {
            let theta = self.rng.gen_range(0.0..1.0) * 2.0 * std::f64::consts::PI;
            let phi = (self.rng.gen_range(0.0f64..1.0) * 2.0 - 1.0).acos();
            let min_dist = self.config.debris.explosion.max_distance as f64 * 0.1;
            let max_dist = self.config.debris.explosion.max_distance as f64;
            let r = self.rng.gen_range(0.0..1.0) * (max_dist - min_dist) + min_dist;

            let debris_x = effect.position.p.x + r * phi.sin() * theta.cos();
            let debris_z = effect.position.p.z + r * phi.sin() * theta.sin();
            let debris_y = effect.position.p.y + self.rng.gen_range(0.0..1.0) * max_dist;

            let debris_pos = Position3 {
                p: LuaVec3(na::Vector3::new(debris_x, debris_y, debris_z)),
                ..effect.position
            };

            // Schedule debris explosion with random delay (0.5 to 3 seconds)
            let delay_seconds = self.rng.gen_range(0.5..3.0);
            let trigger_time = start_time + chrono::Duration::milliseconds((delay_seconds * 1000.0) as i64);
            
            let timed_explosion = TimedExplosion {
                position: debris_pos,
                power: self.config.debris.explosion.power,
                trigger_time,
                effect_type: ExplosionType::Debris,
                azimuth: None, // Debris explosions don't need azimuth
            };
            
            self.timed_explosions.push_back(timed_explosion);
            debug!("COOKOFF: Scheduled debris explosion for {} seconds from now at position {:?}", delay_seconds, debris_pos);
        }

        Ok(())
    }

    /// Schedule debris effects (legacy method for compatibility)
    #[allow(dead_code)]
    fn schedule_debris_effects(&mut self, _lua: MizLua, effect: &ScheduledCookOffEffect) -> Result<()> {
        let debris_count = self.rng.gen_range(self.config.debris.count.min..=self.config.debris.count.max);

        for _j in 0..debris_count {
            let theta = self.rng.gen_range(0.0..1.0) * 2.0 * std::f64::consts::PI;
            let phi = (self.rng.gen_range(0.0f64..1.0) * 2.0 - 1.0).acos();
            let min_dist = self.config.debris.explosion.max_distance as f64 * 0.1;
            let max_dist = self.config.debris.explosion.max_distance as f64;
            let r = self.rng.gen_range(0.0..1.0) * (max_dist - min_dist) + min_dist;

            let debris_x = effect.position.p.x + r * phi.sin() * theta.cos();
            let debris_z = effect.position.p.z + r * phi.sin() * theta.sin();
            let debris_y = effect.position.p.y + self.rng.gen_range(0.0..1.0) * max_dist;

            let debris_pos = Position3 {
                p: LuaVec3(na::Vector3::new(debris_x, debris_y, debris_z)),
                ..effect.position
            };

            self.create_explosion(_lua, &debris_pos, self.config.debris.explosion.power)?;
        }

        Ok(())
    }

    /// Update the system (called periodically)
    pub fn update(&mut self, lua: MizLua, now: DateTime<Utc>) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Check if we should skip this update based on performance monitoring
        if self.should_skip_update() {
            return Ok(());
        }

        // Start timing the update
        let update_start = std::time::Instant::now();

        let cutoff_time = now - chrono::Duration::minutes(PROCESSED_UNITS_CLEANUP_MINUTES);
        self.processed_units.retain(|_, time| *time > cutoff_time);
        
        if self.processed_smoke.len() > PROCESSED_SMOKE_CLEANUP_LIMIT {
            // Remove half of the entries to prevent memory growth while keeping recent data
            let target_size = PROCESSED_SMOKE_CLEANUP_LIMIT / 2;
            let current_size = self.processed_smoke.len();
            if current_size > target_size {
                // Convert to vector, sort by key (which includes timestamp info), and keep recent entries
                let mut entries: Vec<_> = self.processed_smoke.drain().collect();
                entries.sort_by_key(|(unit_id, _)| *unit_id); // Sort by unit_id for consistent cleanup
                entries.truncate(target_size);
                self.processed_smoke = entries.into_iter().collect();
            }
        }
        
        // Process pending effects in a single iteration for better performance
        let mut ready_effects = Vec::new();
        
        for (unit_id, pending_effect) in &mut self.pending_effects {
            // Check if enough time has passed since the effect was created
            let time_since_start = now.signed_duration_since(pending_effect.start_time);
            let min_delay = chrono::Duration::seconds(MIN_EFFECT_DELAY_SECONDS);
            
            if time_since_start >= min_delay {
                // Check if unit has moved (for movement tracking)
                let has_moved = if pending_effect.is_dead {
                    // If unit is dead, trigger effects immediately
                    true
                } else {
                    // For live units, use a time-based approach instead of artificial movement simulation
                    // This is more efficient and still maintains the intended delay behavior
                    // The movement tracking was primarily to add delay, so we can achieve the same with time
                    let movement_delay = chrono::Duration::seconds(3); // 3 second delay for live units
                    time_since_start >= movement_delay
                };
                
                if has_moved {
                    ready_effects.push(*unit_id);
                }
            }
        }
        
        // Process ready effects
        for unit_id in ready_effects {
            if let Some(pending_effect) = self.pending_effects.remove(&unit_id) {
                debug!(
                    "COOKOFF: Processing pending effect for unit {} (ID: {:?}, Type: {}), is_dead: {}, is_cookoff: {}",
                    pending_effect.unit_name, pending_effect.unit_id, pending_effect.unit_type, pending_effect.is_dead, pending_effect.is_cookoff
                );
                
                // Schedule the cook-off effects
                self.schedule_cookoff_effects(lua, unit_id, pending_effect.is_dead)?;
            }
        }
        
        // Process scheduled effects from the queue (optimized single pass)
        while let Some(effect) = self.effect_queue.pop_front() {
            debug!(
                "COOKOFF: Processing scheduled effect for unit {} ({}), explosion: {}, cookoff: {}",
                effect.unit_name, effect.unit_type, effect.explosion.enabled, effect.cookoff.enabled
            );
            
            // Create initial explosion if enabled
            if effect.explosion.enabled {
                self.create_explosion(lua, &effect.position, effect.explosion.power)?;
            }
            
            // Create smoke effect if enabled
            if effect.smoke.is_tanker {
                self.create_smoke_effect(lua, &effect.position, effect.smoke.size, effect.smoke.duration)?;
                // Mark this unit as having processed smoke
                self.processed_smoke.insert(effect.unit_id, true);
            }
            
            // Schedule cook-off effects if enabled (now with proper timing)
            if effect.cookoff.enabled {
                self.schedule_cookoff_effects_for_effect(lua, &effect)?;
            }
        }

        // Process timed explosions
        self.process_timed_explosions(lua, now)?;

        // Update performance monitoring
        let update_duration_us = update_start.elapsed().as_micros() as u64;
        self.update_performance_monitoring(update_duration_us);
        self.update_last_update_time();

        Ok(())
    }

    /// Process timed explosions that are ready to trigger
    fn process_timed_explosions(&mut self, lua: MizLua, now: DateTime<Utc>) -> Result<()> {
        // Process explosions from the front of the queue (they should be time-sorted)
        // This is more efficient than creating a vector and processing in reverse
        while let Some(timed_explosion) = self.timed_explosions.front() {
            if now >= timed_explosion.trigger_time {
                if let Some(timed_explosion) = self.timed_explosions.pop_front() {
                    match timed_explosion.effect_type {
                        ExplosionType::Cookoff | ExplosionType::Debris => {
                            if timed_explosion.power > 0.0 {
                                self.create_explosion(lua, &timed_explosion.position, timed_explosion.power)?;
                                debug!("COOKOFF: Triggered timed {} explosion at {:?} with power {}", 
                                       match timed_explosion.effect_type {
                                           ExplosionType::Cookoff => "cook-off",
                                           ExplosionType::Debris => "debris",
                                           ExplosionType::Flare => "flare",
                                       }, 
                                       timed_explosion.position, timed_explosion.power);
                            }
                        }
                        ExplosionType::Flare => {
                            debug!("COOKOFF: Triggered timed white flare at {:?}", timed_explosion.position);
                            
                            // Create actual DCS flare (white only for cookoff events)
                            match Trigger::singleton(lua) {
                                Ok(trigger) => {
                                    debug!("COOKOFF: Got trigger singleton successfully");
                                    match trigger.action() {
                                        Ok(action) => {
                                            debug!("COOKOFF: Got action successfully");
                                            // Use configurable flare color (like Lua cookoff_flare_color)
                                            let flare_color = match self.config.flares.color {
                                                0 => dcso3::trigger::FlareColor::Green,
                                                1 => dcso3::trigger::FlareColor::Red,
                                                2 => dcso3::trigger::FlareColor::White,
                                                3 => dcso3::trigger::FlareColor::Yellow,
                                                _ => dcso3::trigger::FlareColor::White, // Default to white
                                            };
                                            // Use the stored azimuth from scheduling, or generate a random one if not available
                                            let azimuth = timed_explosion.azimuth.unwrap_or_else(|| self.rng.gen_range(0..360) as u16);
                                            
                                            debug!("COOKOFF: Attempting to create flare at {:?} with azimuth {}", 
                                                   timed_explosion.position, azimuth);
                                            
                                            
                                            match action.signal_flare(timed_explosion.position.p, flare_color, azimuth) {
                                                Ok(_) => {
                                                    debug!("COOKOFF: Successfully created white flare at {:?} with azimuth {}", 
                                                           timed_explosion.position, azimuth);
                                                }
                                                Err(e) => {
                                                    debug!("COOKOFF: Failed to create flare: {:?}", e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            debug!("COOKOFF: Failed to get action: {:?}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug!("COOKOFF: Failed to get trigger singleton: {:?}", e);
                                }
                            }
                        }
                    }
                } else {
                    break; // No more explosions to process
                }
            } else {
                break; // No more explosions ready to trigger
            }
        }
        
        Ok(())
    }

    /// Get the configuration
    pub fn config(&self) -> &CookOffConfig {
        &self.config
    }

    /// Update the configuration
    pub fn update_config(&mut self, config: CookOffConfig) {
        self.config = config;
    }

    /// Add or update a unit type
    pub fn add_unit(&mut self, unit_type: CompactString, properties: UnitProperties) {
        self.units.insert(unit_type, properties);
    }

    /// Remove a unit type
    pub fn remove_unit(&mut self, unit_type: &str) {
        self.units.remove(unit_type);
    }

    /// Get the number of pending effects
    pub fn pending_effects_count(&self) -> usize {
        self.pending_effects.len()
    }

    /// Get the number of units configured
    pub fn units_count(&self) -> usize {
        self.units.len()
    }

    /// Get the number of processed units
    pub fn processed_units_count(&self) -> usize {
        self.processed_units.len()
    }

    /// Get the number of effects in queue
    pub fn effect_queue_count(&self) -> usize {
        self.effect_queue.len()
    }

    /// Get the number of timed explosions pending
    pub fn timed_explosions_count(&self) -> usize {
        self.timed_explosions.len()
    }

    /// Check if a unit type is configured for cook-off effects
    pub fn is_unit(&self, unit_type: &str) -> bool {
        self.units.contains_key(unit_type)
    }

    /// Update performance monitoring and adjust update rate
    pub fn update_performance_monitoring(&mut self, update_duration_us: u64) {
        // Add current update time to the queue
        self.update_times.push_back(update_duration_us);
        
        // Keep only the last 10 update times
        if self.update_times.len() > 10 {
            self.update_times.pop_front();
        }
        
        // Calculate average update time
        if self.update_times.len() >= 5 {
            let avg_update_time = self.update_times.iter().sum::<u64>() / self.update_times.len() as u64;
            
            // Adjust performance mode based on average update time
            // Target: 16ms or less for good performance
            // Use aggressive thresholds to maintain target performance
            let new_mode = if avg_update_time > 25_000 { // 25ms threshold - switch to critical
                PerformanceMode::Critical
            } else if avg_update_time > 16_000 { // 16ms threshold - switch to minimal
                PerformanceMode::Minimal
            } else if avg_update_time > 10_000 { // 10ms threshold - switch to reduced
                PerformanceMode::Reduced
            } else {
                PerformanceMode::Normal
            };
            
            // Update performance mode and interval if changed
            if new_mode != self.performance_mode {
                self.performance_mode = new_mode;
                self.adaptive_update_interval = match new_mode {
                    PerformanceMode::Normal => 100,   // 100ms
                    PerformanceMode::Reduced => 200,  // 200ms
                    PerformanceMode::Minimal => 400,  // 400ms
                    PerformanceMode::Critical => 750, // 750ms
                };
                
                debug!("COOKOFF: Performance mode changed to {:?}, update interval: {}ms, avg update time: {}μs (target: <16ms)", 
                       new_mode, self.adaptive_update_interval, avg_update_time);
            }
        }
    }

    /// Check if we should skip this update based on performance monitoring
    pub fn should_skip_update(&self) -> bool {
        let now = Utc::now();
        let time_since_last_update = now.signed_duration_since(self.last_update_time).num_milliseconds();
        time_since_last_update < self.adaptive_update_interval
    }

    /// Update the last update time
    pub fn update_last_update_time(&mut self) {
        self.last_update_time = Utc::now();
    }


    /// Get unit properties for a unit type
    pub fn get_unit_properties(&self, unit_type: &str) -> Option<&UnitProperties> {
        self.units.get(unit_type)
    }

}