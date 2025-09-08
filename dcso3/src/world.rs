/*
Copyright 2024 Eric Stokes.

This file is part of dcso3.

dcso3 is free software: you can redistribute it and/or modify it under
the terms of the MIT License.

dcso3 is distributed in the hope that it will be useful, but WITHOUT
ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
FITNESS FOR A PARTICULAR PURPOSE.
*/

use super::{as_tbl, event::Event, unit::Unit, wrap_f, String};
use crate::{
    airbase::Airbase,
    atomic_id, cvt_err,
    env::miz::GroupId,
    object::{Object, ObjectCategory},
    trigger::{MarkId, SideFilter},
    wrapped_table, LuaEnv, LuaVec3, MizLua, Position3, Sequence, Time,
};
use anyhow::Result;
use compact_str::format_compact;
use log::warn;
use mlua::{prelude::*, Value};
use serde_derive::{Deserialize, Serialize};
use std::ops::Deref;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SearchVolume {
    Segment {
        from: LuaVec3,
        to: LuaVec3,
    },
    Box {
        min: LuaVec3,
        max: LuaVec3,
    },
    Sphere {
        point: LuaVec3,
        radius: f64,
    },
    Pyramid {
        pos: Position3,
        length: f32,
        half_angle_hor: f32,
        half_angle_ver: f32,
    },
}

impl<'lua> IntoLua<'lua> for SearchVolume {
    fn into_lua(self, lua: &'lua Lua) -> LuaResult<Value<'lua>> {
        let tbl = lua.create_table()?;
        let params = lua.create_table()?;
        match self {
            Self::Segment { from, to } => {
                tbl.raw_set("id", 0)?;
                params.raw_set("from", from)?;
                params.raw_set("to", to)?;
            }
            Self::Box { min, max } => {
                tbl.raw_set("id", 1)?;
                params.raw_set("min", min)?;
                params.raw_set("max", max)?;
            }
            Self::Sphere { point, radius } => {
                tbl.raw_set("id", 2)?;
                params.raw_set("point", point)?;
                params.raw_set("radius", radius)?;
            }
            Self::Pyramid {
                pos,
                length,
                half_angle_hor,
                half_angle_ver,
            } => {
                tbl.raw_set("id", 3)?;
                params.raw_set("pos", pos)?;
                params.raw_set("length", length)?;
                params.raw_set("halfAngleHor", half_angle_hor)?;
                params.raw_set("halfAngleVer", half_angle_ver)?;
            }
        }
        tbl.raw_set("params", params)?;
        Ok(Value::Table(tbl))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MarkPanel<'lua> {
    pub id: MarkId,
    pub time: Time,
    pub initiator: Option<Unit<'lua>>,
    pub side: SideFilter,
    pub group_id: Option<GroupId>,
    pub text: String,
    pub pos: LuaVec3,
}

impl<'lua> FromLua<'lua> for MarkPanel<'lua> {
    fn from_lua(value: Value<'lua>, lua: &'lua Lua) -> LuaResult<Self> {
        let tbl = LuaTable::from_lua(value, lua)?;
        Ok(Self {
            id: tbl.raw_get("idx")?,
            time: tbl.raw_get("time")?,
            initiator: tbl.raw_get("initiator")?,
            side: match tbl.raw_get::<_, i64>("coalition")? {
                -1 | 255 => SideFilter::All,
                0 => SideFilter::Neutral,
                1 => SideFilter::Red,
                2 => SideFilter::Blue,
                _ => return Err(cvt_err("side filter")),
            },
            group_id: match tbl.raw_get::<_, i64>("groupID")? {
                -1 => None,
                n => Some(GroupId::from(n)),
            },
            text: tbl.raw_get("text")?,
            pos: tbl.raw_get("pos")?,
        })
    }
}

atomic_id!(HandlerId);

impl HandlerId {
    fn key(&self) -> String {
        String(format_compact!("rustHandler{}", self.0))
    }
}

wrapped_table!(World, None);

impl<'lua> World<'lua> {
    pub fn singleton(lua: MizLua<'lua>) -> Result<Self> {
        Ok(lua.inner().globals().raw_get("world")?)
    }

    pub fn add_event_handler<F>(&self, f: F) -> Result<HandlerId>
    where
        F: Fn(MizLua<'lua>, Event) -> Result<()> + 'static,
    {
        let globals = self.lua.globals();
        let id = HandlerId::new();
        let tbl = self.lua.create_table()?;
        tbl.set(
            "onEvent",
            self.lua
                .create_function(move |lua, (_, ev): (Value, Value)| {
                    match Event::from_lua(ev, lua) {
                        Ok(ev) => wrap_f("event handler", MizLua(lua), |lua| f(lua, ev)),
                        Err(e) => {
                            warn!("error translating event: {:?}", e);
                            Ok(())
                        }
                    }
                })?,
        )?;
        self.t.call_function::<_, ()>("addEventHandler", tbl.clone())?;
        globals.raw_set(id.key(), tbl)?;
        Ok(id)
    }

    pub fn remove_event_handler(&self, id: HandlerId) -> Result<()> {
        let globals = self.lua.globals();
        let key = id.key();
        let handler = globals.raw_get(key.clone())?;
        let handler = as_tbl("EventHandler", None, handler)?;
        self.t.call_function::<_, ()>("removeEventHandler", handler)?;
        globals.raw_remove(key)?;
        Ok(())
    }

    pub fn get_player(&self) -> Result<Sequence<'lua, Unit<'lua>>> {
        Ok(self.t.call_function("getPlayer", ())?)
    }

    pub fn get_airbases(&self) -> Result<Sequence<'lua, Airbase<'lua>>> {
        Ok(self.t.call_function("getAirbases", ())?)
    }

    pub fn search_objects<F, T>(
        &self,
        category: ObjectCategory,
        volume: SearchVolume,
        arg: T,
        f: F,
    ) -> Result<()>
    where
        T: IntoLua<'lua> + FromLua<'lua>,
        F: Fn(MizLua, Object<'lua>, T) -> Result<bool> + 'static,
    {
        let f = self
            .lua
            .create_function(move |lua, (o, arg): (Object, T)| {
                wrap_f("searchObjects", MizLua(lua), |lua| f(lua, o, arg))
            })?;
        Ok(self
            .t
            .call_function("searchObjects", (category, volume, f, arg))?)
    }

    /// Search for objects and return them as a collection
    /// This is more convenient than the closure-based approach for many use cases
    pub fn search_objects_collect(
        &self,
        category: ObjectCategory,
        volume: SearchVolume,
    ) -> Result<Vec<Object<'lua>>> {
        // Create a table to store the results
        let results_table = self.lua.create_table()?;
        let count = 0i32;
        
        let f = self
            .lua
            .create_function(move |lua, (o, _count): (Object, i32)| {
                // Store the object in the results table
                let results: LuaTable = lua.globals().raw_get("__search_results")?;
                let current_count: i32 = lua.globals().raw_get("__search_count")?;
                results.raw_set(current_count + 1, o)?;
                lua.globals().raw_set("__search_count", current_count + 1)?;
                Ok(true) // Continue searching
            })?;
            
        // Set up global variables for the search
        self.lua.globals().raw_set("__search_results", results_table.clone())?;
        self.lua.globals().raw_set("__search_count", 0i32)?;
        
        // Perform the search
        self.t.call_function::<_, ()>("searchObjects", (category, volume, f, count))?;
        
        // Collect the results
        let final_count: i32 = self.lua.globals().raw_get("__search_count")?;
        let mut objects = Vec::new();
        
        for i in 1..=final_count {
            if let Ok(obj) = results_table.raw_get::<_, Object>(i) {
                objects.push(obj);
            }
        }
        
        // Clean up global variables
        self.lua.globals().raw_remove("__search_results")?;
        self.lua.globals().raw_remove("__search_count")?;
        
        Ok(objects)
    }

    /// Search for objects using the exact same approach as the Lua script
    /// This matches the Lua script's world.searchObjects(Object.Category.UNIT, volS, ifFound) pattern
    pub fn search_objects_lua_style(
        &self,
        category: ObjectCategory,
        point: LuaVec3,
        radius: f64,
    ) -> Result<Vec<Object<'lua>>> {
        // Create the volume exactly like the Lua script does
        let vol_s = self.lua.create_table()?;
        vol_s.raw_set("id", 2)?; // world.VolumeType.SPHERE = 2
        
        let params = self.lua.create_table()?;
        params.raw_set("point", point)?;
        params.raw_set("radius", radius)?;
        vol_s.raw_set("params", params)?;
        
        // Create a table to store the results
        let results_table = self.lua.create_table()?;
        let count = 0i32;
        
        let f = self
            .lua
            .create_function(move |lua, (o, _count): (Object, i32)| {
                // Store the object in the results table
                let results: LuaTable = lua.globals().raw_get("__search_results")?;
                let current_count: i32 = lua.globals().raw_get("__search_count")?;
                results.raw_set(current_count + 1, o)?;
                lua.globals().raw_set("__search_count", current_count + 1)?;
                Ok(true) // Continue searching
            })?;
            
        // Set up global variables for the search
        self.lua.globals().raw_set("__search_results", results_table.clone())?;
        self.lua.globals().raw_set("__search_count", 0i32)?;
        
        // Perform the search using the exact same call as the Lua script
        self.t.call_function::<_, ()>("searchObjects", (category, vol_s, f, count))?;
        
        // Collect the results
        let final_count: i32 = self.lua.globals().raw_get("__search_count")?;
        let mut objects = Vec::new();
        
        for i in 1..=final_count {
            if let Ok(obj) = results_table.raw_get::<_, Object>(i) {
                objects.push(obj);
            }
        }
        
        // Clean up global variables
        self.lua.globals().raw_remove("__search_results")?;
        self.lua.globals().raw_remove("__search_count")?;
        
        Ok(objects)
    }

    /// Search for objects in multiple categories like the Lua script does
    /// This matches the Lua script's pattern of searching UNIT, STATIC, SCENERY, CARGO
    pub fn search_objects_multi_category(
        &self,
        point: LuaVec3,
        radius: f64,
    ) -> Result<Vec<Object<'lua>>> {
        let mut all_objects = Vec::new();
        
        // Search all categories like the Lua script does
        let categories = [
            ObjectCategory::Unit,
            ObjectCategory::Static,
            ObjectCategory::Scenery,
            ObjectCategory::Cargo,
        ];
        
        for category in categories.iter() {
            let objects = self.search_objects_lua_style(*category, point, radius)?;
            all_objects.extend(objects);
        }
        
        Ok(all_objects)
    }

    pub fn remove_junk(&self, volume: SearchVolume) -> Result<i64> {
        Ok(self.t.call_function("removeJunk", volume)?)
    }

    pub fn get_mark_panels(&self) -> Result<Sequence<'lua, MarkPanel<'lua>>> {
        Ok(self.t.call_function("getMarkPanels", ())?)
    }
}
