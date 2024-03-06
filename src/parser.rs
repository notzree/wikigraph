use crate::models::{LookupEntry, RedirectEntry};
use crate::multipeek::MultiPeek;
use crate::schema::lookup::dsl::*;
use crate::schema::redirect::dsl::*;
use byteorder::{LittleEndian, WriteBytesExt};
use core::panic;
use diesel::insert_into;
use diesel::pg::PgConnection;
use diesel::result::{DatabaseErrorKind, Error::DatabaseError};
use diesel::{connection, prelude::*};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Error, Seek, SeekFrom, Write};
use std::{cmp::min, fmt::Write as fmtWrite};

//All sizes are in bytes. ie: 4 * 4 = 16 bytes = 4 integers.
const FILE_HEADER_SIZE: usize = 4 * 4;
const NODE_HEADER_SIZE: usize = 4 * 4;
const LINK_SIZE: usize = 4;
const LEFT_BRACE: char = '[';
const RIGHT_BRACE: char = ']';
const ADJACENCY_LIST_PATH: &str = "adjacency_list.txt";
const NUM_ARTICLES: u64 = 6789472;

pub struct Parser {
    file_reader: quick_xml::Reader<std::io::BufReader<File>>,
    output_file_path: String,
    connection_string: String,
    count: i32,
}

impl Parser {
    pub fn new(file: std::fs::File, output_file_path: String, db_url: String) -> Parser {
        let mut file_reader = Reader::from_reader(BufReader::new(file));
        file_reader.trim_text(true);
        Parser {
            file_reader,
            output_file_path,
            connection_string: db_url,
            count: 0,
        }
    }
    pub fn set_count(&mut self, count: i32) {
        self.count = count;
    }
    //First pass to generate lookup table with computed byte offsets + create text file with adjacency list
    pub fn pre_process_file(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let bar = ProgressBar::new(NUM_ARTICLES);
        bar.set_style(
            ProgressStyle::with_template(
                "[{wide_bar:.cyan/blue}] [{elapsed_precise}] {pos:>7}/{len:7} ({eta})",
            )
            .unwrap()
            .with_key("eta", |state: &ProgressState, w: &mut dyn fmtWrite| {
                write!(w, "{:.1}hrs", state.eta().as_secs_f64() / 3600.0).unwrap()
            })
            .progress_chars("#>-"),
        );
        let mut adj_list = File::create(ADJACENCY_LIST_PATH).unwrap();

        let mut connection = PgConnection::establish(&self.connection_string)
            .unwrap_or_else(|_| panic!("Error connecting to {}", self.connection_string));
        let mut buf: Vec<u8> = Vec::new();
        let mut prev_offset: usize = FILE_HEADER_SIZE;
        let mut prev_length: usize = 0;
        let mut count = 0;

        loop {
            match self.file_reader.read_event_into(&mut buf) {
                Err(e) => panic!(
                    "Error at position {}: {:?}",
                    self.file_reader.buffer_position(),
                    e
                ),
                // exits the loop when reaching end of file
                Ok(Event::Eof) => {
                    self.set_count(count);
                    println!("Count: {}", count);
                    bar.finish();
                    return Ok(());
                }
                Ok(Event::Start(e)) => {
                    if e.name().as_ref() == b"page" {
                        let mut page_title = String::new();
                        let mut page_txt: String = String::new();
                        let mut is_redirect: bool = false;
                        buf.clear();
                        loop {
                            match self.file_reader.read_event_into(&mut buf) {
                                Ok(Event::Start(e)) => {
                                    if e.name().as_ref() == b"title" {
                                        let text_event = self.file_reader.read_event_into(&mut buf);
                                        if let Ok(Event::Text(e)) = text_event {
                                            if e.unescape()
                                                .unwrap()
                                                .into_owned()
                                                .contains("Wikipedia:")
                                            {
                                                break;
                                            }
                                            page_title = e.unescape().unwrap().into_owned();
                                        }

                                        continue;
                                    }
                                    if e.name().as_ref() == b"text" {
                                        let text_event = self.file_reader.read_event_into(&mut buf);
                                        if let Ok(Event::Text(e)) = text_event {
                                            page_txt = e.unescape().unwrap().into_owned();
                                        }
                                    }
                                    continue;
                                }
                                Ok(Event::End(e)) => {
                                    if e.name().as_ref() == b"page" {
                                        //Reached </page> tag
                                        break;
                                    }
                                }
                                Ok(Event::Eof) => break,
                                Ok(Event::Empty(e)) => {
                                    if e.name().as_ref() == b"redirect" {
                                        is_redirect = true;
                                        continue;
                                    }
                                }
                                _ => (),
                            }
                            buf.clear();
                        }
                        if page_title.is_empty()
                            || page_txt.is_empty()
                            || page_title.contains("Template:")
                            || page_title.contains("Wikipedia:")
                            || page_title.contains("File:")
                            || page_title.contains("WP:")
                            || page_title.contains("User:")
                            || page_title.contains("Help:")
                            || page_title.contains("Draft:")
                            || page_title.len() > 255
                            || page_title.contains("(disambiguation)")
                            || page_txt.contains("{{disambiguation}}")
                            || page_txt.contains("{{disambig")
                        {
                            //skip redirect pages, empty pages, templates, wikipedia pages, files, user pages, help pages, disambiguation pages
                            continue;
                        }
                        if is_redirect {
                            //TODO: Create a seperate table in the db to store redirects so we can use them later on...
                            //First we must grab the to redirect pages.
                            let redirect_output = self.extract_links_from_text(page_txt);
                            if redirect_output.is_empty() {
                                println!("{} skipped", page_title);
                                continue;
                            }
                            let redirect_entry = RedirectEntry {
                                redirect_from: page_title.clone(),
                                redirect_to: redirect_output[0].clone(),
                            };
                            self.add_redirect_entry(&mut connection, &redirect_entry)
                                .unwrap();
                            continue;
                        }
                        let links = self.extract_links_from_text(page_txt);
                        let curr_length = self.compute_length(links.len());
                        prev_offset = self.compute_byte_offset(prev_offset, prev_length);
                        let lookup_entry = LookupEntry {
                            title: page_title.clone(),
                            byteoffset: prev_offset.try_into().unwrap(), // in bytes
                            length: curr_length.try_into().unwrap(),
                        };
                        match self.add_lookup_entry(&mut connection, &lookup_entry) {
                            Ok(_) => (),
                            Err(e) => panic!("Error adding to lookup table: {:?}", e),
                        }
                        match self.add_to_adj_list(&page_title, links, &mut adj_list) {
                            Ok(_) => (),
                            Err(e) => panic!("Error adding to lookup table: {:?}", e),
                        }
                        bar.inc(1);
                        prev_length = curr_length;
                        count += 1;
                    }
                }

                // There are several other `Event`s we do not consider here
                _ => (),
            }
            // if we don't keep a borrow elsewhere, we can clear the buffer to keep memory usage low
            buf.clear();
        }
    }
    //Second pass to take adjacency list + lookup table -> graph in binary format.
    pub fn create_graph(&self) {
        const FILE_VERSION: i32 = 1;
        let bar = ProgressBar::new(NUM_ARTICLES);
        bar.set_style(
            ProgressStyle::with_template(
                "[{wide_bar:.cyan/blue}] [{elapsed_precise}] {pos:>7}/{len:7} ({eta})",
            )
            .unwrap()
            .with_key("eta", |state: &ProgressState, w: &mut dyn fmtWrite| {
                write!(w, "{:.1}hrs", state.eta().as_secs_f64() / 3600.0).unwrap()
            })
            .progress_chars("#>-"),
        );
        let mut connection = PgConnection::establish(&self.connection_string)
            .unwrap_or_else(|_| panic!("Error connecting to {}", self.connection_string));
        let file = File::open(ADJACENCY_LIST_PATH).unwrap();
        let mut graph = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&self.output_file_path)
            .unwrap();

        //writing file header
        graph.write_i32::<LittleEndian>(FILE_VERSION).unwrap();
        graph.write_i32::<LittleEndian>(self.count).unwrap();
        //2 integers are unused.
        graph.write_i32::<LittleEndian>(0).unwrap();
        graph.write_i32::<LittleEndian>(0).unwrap();
        for line in BufReader::new(file).lines() {
            bar.inc(1);
            match line {
                Ok(line) => {
                    let mut split = line.split('|');
                    let t = split.next().unwrap();
                    let current_position = graph.stream_position().unwrap() - 16; //-16 bytes is only here because I failed to take into account the file header during pre-processing. ?reminder Will remove later on.
                    let lookup_entry = self.look_up_lookup_entry(t, &mut connection).unwrap();
                    if current_position != lookup_entry.byteoffset as u64 {
                        panic!(
                            "Byteoffset mismatch. Expected: {}, Got: {}",
                            lookup_entry.byteoffset, current_position
                        );
                    }
                    let links = split.next().unwrap().split(',');
                    let num_links = links.clone().count() as i32;
                    Self::write_node_header(&mut graph, num_links);
                    for link in links {
                        let link = link.to_string();
                        //Link does not work because of capitalization. Have to think of a way to fix this. Issue is some links are case sensitive, other links are not. Maybe the play is to just remove cases entirely? but idk
                        //TODO: Fix link issue because of capitalization AND pluraization...
                        let lookup_entry =
                            self.look_up_lookup_entry(&link, &mut connection).unwrap();
                        graph
                            .write_i32::<LittleEndian>(lookup_entry.byteoffset)
                            .unwrap();
                    }
                }
                Err(e) => panic!("Error reading line: {:?}", e),
            }
        }
        bar.finish();
    }

    fn extract_links_from_text(&self, text: String) -> Vec<String> {
        let mut links: Vec<String> = Vec::new();
        let mut current_link = String::new();
        let mut inside_link = false;

        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '[' if chars.peek() == Some(&'[') => {
                    //detect starting links
                    // Detect starting "[["
                    chars.next(); // Skip the next '[' as it's part of the marker
                                  // if chars.peek() == Some(&':') { //I believe we already check for Wikipedia: in the pre-processing stage, so we can skip this check.
                                  //     //skip wikipedia links
                                  //     continue;
                                  // }
                    inside_link = true;
                    current_link.clear();
                }
                ']' if chars.peek() == Some(&']') => {
                    //end links
                    // Detect ending "]]"
                    chars.next(); // Skip the next ']' as it's part of the marker
                    if inside_link {
                        if current_link.contains('|') {
                            let mut split = current_link.split('|');
                            let link = split.next().unwrap();
                            current_link = link.to_string();
                        }
                        if current_link.contains('#') {
                            //Some redirets havee a #and a specific A-tag, we ignore them.
                            let mut split = current_link.split('#');
                            let link = split.next().unwrap();
                            current_link = link.to_string();
                        }
                        links.push(current_link.clone());
                        inside_link = false;
                    }
                }
                '<' => {
                    //we encounter a tag, based on the markup, we can skip the content of the tag...
                    let mut multipeek = MultiPeek::new(chars.clone(), 15);
                    // print!("multipeek value {:?}", multipeek.peek_until(20));
                    if multipeek.peek_until(15).contains("syntaxhighlight") {
                        loop {
                            if multipeek.is_empty() {
                                break;
                            }
                            if multipeek.peek_until(15).contains("</syntaxhighlight>") {
                                break;
                            }
                            multipeek.next();
                        }
                    }
                }
                '{' if chars.peek() == Some(&'{') => {
                    //
                    //link or template, skip until end
                    let mut multipeek = MultiPeek::new(chars.clone(), 2);
                    loop {
                        if multipeek.is_empty() {
                            break;
                        }
                        if multipeek.peek_until(2).contains("}}") {
                            break;
                        }
                        multipeek.next();
                    }
                }

                _ if inside_link => {
                    current_link.push(c);
                    if current_link == "File:"
                        || current_link == "Wikipedia:"
                        || current_link == "WP:"
                        || current_link == "User:"
                    {
                        // we realize that we are in either a file, template, or wikipedia article namespace. We reseet
                        inside_link = false;
                        current_link.clear();
                    }
                }
                _ => {}
            }
        }

        links
    }

    fn add_lookup_entry(
        &self,
        connection: &mut PgConnection,
        lookup_entry: &LookupEntry,
    ) -> Result<(), diesel::result::Error> {
        match insert_into(lookup).values(lookup_entry).execute(connection) {
            Ok(_) => Ok(()),
            Err(e) => panic!(
                "Error adding {} to lookup table: {:?}",
                lookup_entry.title, e
            ),
        }
    }

    fn add_redirect_entry(
        &self,
        connection: &mut PgConnection,
        redirect_entry: &RedirectEntry,
    ) -> Result<(), diesel::result::Error> {
        match insert_into(redirect)
            .values(redirect_entry)
            .execute(connection)
        {
            Ok(_) => Ok(()),
            Err(DatabaseError(DatabaseErrorKind::UniqueViolation, _)) => {
                println!(
                    "Duplicate key error when adding {} | {:?} to redirect table.",
                    redirect_entry.redirect_from, redirect_entry.redirect_to
                );
                Ok(()) //keep going if we encounter a duplicate key error.
            }
            Err(e) => Err(e), // For other errors, we will propgate
        }
    }

    fn add_to_adj_list(
        &self,
        title_entry: &str,
        links: Vec<String>,
        file: &mut File,
    ) -> Result<(), std::io::Error> {
        let mut line = title_entry.to_string() + "|";
        for link in links.iter() {
            line.push_str(link);
            line.push(',');
        }
        line.push('\n');
        let _ = match file.write_all(line.as_bytes()) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };

        Ok(())
    }

    fn look_up_lookup_entry(
        &self,
        matching_title: &str,
        connection: &mut PgConnection,
    ) -> Result<LookupEntry, diesel::result::Error> {
        lookup.filter(title.eq(matching_title)).first(connection)
    }

    fn lookup_redirect_entry(
        &self,
        matching_title: &str,
        connection: &mut PgConnection,
    ) -> Result<Option<RedirectEntry>, diesel::result::Error> {
        //returns an option because not finding a redirect entry is not an error.
        redirect
            .filter(redirect_from.eq(matching_title)) //todo: fix this shit by reverting it back to where clause, have to go around and make sure everything is .to_lowercase();
            .first(connection)
            .optional()
    }

    fn lookup_with_redirects(
        &self,
        input_title: &str,
        connection: &mut PgConnection,
    ) -> Result<LookupEntry, diesel::result::Error> {
        //abstracts the logic of checking if redirect exists when searching a link in the adjacency list in the lookup table
        //we first check if it is a redirect, if it is, we lookup the corresponding title in the lookup table and return it.
        let redirect_entry = self.lookup_redirect_entry(input_title, connection).unwrap();
        match redirect_entry {
            Some(redirect_entry) => {
                let lookup_entry = self
                    .look_up_lookup_entry(&redirect_entry.redirect_to, connection)
                    .unwrap();
                Ok(lookup_entry)
            }
            None => {
                //no redirect exists, should just be a link
                let lookup_entry = self.look_up_lookup_entry(input_title, connection).unwrap();
                Ok(lookup_entry)
            }
        }
    }
    fn write_node_header(graph: &mut File, num_links: i32) {
        //3 integers are unused. The number of links is the 4th integer. first integer is used for traversal.
        graph.write_i32::<LittleEndian>(0).unwrap();
        graph.write_i32::<LittleEndian>(0).unwrap();
        graph.write_i32::<LittleEndian>(0).unwrap();
        graph.write_i32::<LittleEndian>(num_links).unwrap();
    }

    fn compute_byte_offset(&self, prev_offset: usize, prev_length: usize) -> usize {
        prev_offset + prev_length
    }

    fn compute_length(&self, num_links: usize) -> usize {
        NODE_HEADER_SIZE + num_links * LINK_SIZE
    }
}
