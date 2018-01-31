use iron::typemap::Key;
use serde_json;
use std::collections::HashMap;
use std::error::Error;

use super::db::PostgresConnection;

// https://github.com/mapbox/postgis-vt-util/blob/master/src/TileBBox.sql
fn tilebbox(z: u32, x: u32, y: u32) -> String {
    let max = 20037508.34;
    let res = (max * 2.0) / (2_i32.pow(z) as f64);

    let xmin = -max + (x as f64 * res);
    let ymin = max - (y as f64 * res);
    let xmax = -max + (x as f64 * res) + res;
    let ymax = max - (y as f64 * res) - res;

    format!("ST_MakeEnvelope({0}, {1}, {2}, {3}, 3857)", xmin, ymin, xmax, ymax)
}

fn transform_to_mercator(geom: String, srid: u32) -> String {
    if srid == 3857 {
        geom
    } else {
        format!("ST_Transform({0}, 3857)", geom)
    }
}

fn query_properties(properties: HashMap<String, String>) -> String {
    let keys: Vec<String> = properties
        .keys()
        .map(|key| format!("'{0}', {0}", key))
        .collect();

    format!("jsonb_strip_nulls(jsonb_build_object({0}))", keys.join(","))
}

#[derive(Serialize, Debug)]
pub struct Tileset {
    pub id: String,
    schema: String,
    table: String,
    geometry_column: String,
    srid: u32,
    extent: u32,
    buffer: u32,
    clip_geom: bool,
    geometry_type: String,
    properties: HashMap<String, String>
}

impl Tileset {
    pub fn get_query(&self, z: u32, x: u32, y: u32, condition: Option<String>) -> String {
        let mercator_bounds = tilebbox(z, x, y);

        let original_bounds = if self.srid == 3857 {
            mercator_bounds.clone()
        } else {
            format!("ST_Transform({0}, {1})", mercator_bounds, self.srid)
        };

        let query = format!(
            "WITH bounds AS (SELECT {mercator_bounds} as mercator, {original_bounds} as original) \
            SELECT ST_AsMVT(tile, '{id}', {extent}, 'geom') FROM (\
                SELECT \
                    ST_AsMVTGeom(\
                        {geometry_column_mercator},\
                        bounds.mercator,\
                        {extent},\
                        {buffer},\
                        {clip_geom}\
                    ) AS geom,\
                    {properties} as properties \
                FROM {id}, bounds \
                WHERE {geometry_column} && bounds.original {condition}\
            ) AS tile WHERE geom IS NOT NULL",
            id=self.id,
            geometry_column=self.geometry_column,
            geometry_column_mercator=transform_to_mercator(self.geometry_column.clone(), self.srid),
            original_bounds=original_bounds,
            mercator_bounds=mercator_bounds,
            extent=self.extent,
            buffer=self.buffer,
            clip_geom=self.clip_geom,
            properties=query_properties(self.properties.clone()),
            condition=condition.map_or("".to_string(), |condition| format!("AND {}", condition)),
        );

        debug!("\n\n{}\n\n", query);
        query
    }
}

pub struct Tilesets;
impl Key for Tilesets { type Value = HashMap<String, Tileset>; }

fn value_to_hashmap(value: serde_json::Value) -> HashMap<String, String> {
    let mut hashmap = HashMap::new();

    let object = value.as_object().unwrap();
    for (key, value) in object {
        let string_value = value.as_str().unwrap();
        hashmap.insert(key.to_string(), string_value.to_string());
    };

    hashmap
}

pub fn get_tilesets(conn: PostgresConnection) -> Result<HashMap<String, Tileset>, Box<Error>> {
    let query = "
        SELECT
            f_table_schema, f_table_name, f_geometry_column, srid, type,
            json_object_agg(columns.column_name, columns.udt_name) as properties
        FROM geometry_columns
        LEFT JOIN information_schema.columns AS columns ON
            geometry_columns.f_table_schema = columns.table_schema AND
            geometry_columns.f_table_name = columns.table_name AND
            geometry_columns.f_geometry_column != columns.column_name
        GROUP BY f_table_schema, f_table_name, f_geometry_column, srid, type";

    let default_extent = 4096;
    let default_buffer = 0; // 256
    let default_clip_geom = true;

    let mut tilesets = HashMap::new();
    let rows = try!(conn.query(&query, &[]));

    for row in &rows {
        let schema: String = row.get("f_table_schema");
        let table: String = row.get("f_table_name");
        let id = format!("{}.{}", schema, table);

        let geometry_column: String = row.get("f_geometry_column");
        let srid: i32 = row.get("srid");

        let tileset = Tileset {
            id: id.to_string(),
            schema: schema,
            table: table,
            geometry_column: geometry_column,
            srid: srid as u32,
            extent: default_extent,
            buffer: default_buffer,
            clip_geom: default_clip_geom,
            geometry_type: row.get("type"),
            properties: value_to_hashmap(row.get("properties"))
        };

        tilesets.insert(id, tileset);
    }

    Ok(tilesets)
}
