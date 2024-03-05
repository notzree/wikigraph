use crate::schema::*;
use diesel::prelude::*;
use serde::Serialize;

#[derive(Insertable, Queryable, Serialize, Debug)]
#[table_name = "lookup"]
pub struct LookupEntry {
    pub title: String,
    pub byteoffset: i32,
    pub length: i32,
}

#[derive(Insertable, Queryable, Serialize, Debug)]
#[table_name = "redirect"]
pub struct RedirectEntry {
    pub redirect_from: String,
    pub redirect_to: String,
}
