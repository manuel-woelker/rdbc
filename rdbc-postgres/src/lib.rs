//! Postgres RDBC Driver
//!
//! This crate implements an RDBC Driver for the `postgres` crate.
//!
//! The RDBC (Rust DataBase Connectivity) API is loosely based on the ODBC and JDBC standards.
//!
//! ```rust,ignore
//! use rdbc::Value;
//! use rdbc_postgres::PostgresDriver;
//! let driver = PostgresDriver::new();
//! let conn = driver.connect("postgres://postgres:password@localhost:5433").unwrap();
//! let mut conn = conn.borrow_mut();
//! let stmt = conn.prepare("SELECT a FROM b WHERE c = ?").unwrap();
//! let mut stmt = stmt.borrow_mut();
//! let rs = stmt.execute_query(&vec![Value::Int32(123)]).unwrap();
//! let mut rs = rs.borrow_mut();
//! while rs.next() {
//!   println!("{:?}", rs.get_string(1));
//! }
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use postgres;
use postgres::rows::Rows;
use postgres::{Connection, TlsMode};

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Token, Tokenizer, Word};

use rdbc;

/// Convert a Postgres error into an RDBC error
fn to_rdbc_err(e: &postgres::error::Error) -> rdbc::Error {
    rdbc::Error::General(format!("{:?}", e))
}

pub struct PostgresDriver {}

impl PostgresDriver {
    pub fn new() -> Self {
        PostgresDriver {}
    }

    pub fn connect(&self, url: &str) -> rdbc::Result<Rc<RefCell<dyn rdbc::Connection>>> {
        postgres::Connection::connect(url, TlsMode::None)
            .map_err(|e| to_rdbc_err(&e))
            .map(|c| {
                Ok(Rc::new(RefCell::new(PConnection::new(c))) as Rc<RefCell<dyn rdbc::Connection>>)
            })?
    }
}

struct PConnection {
    conn: Connection,
}

impl PConnection {
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }
}

impl rdbc::Connection for PConnection {
    fn prepare(&mut self, sql: &str) -> rdbc::Result<Rc<RefCell<dyn rdbc::Statement + '_>>> {
        // translate SQL, mapping ? into $1 style bound param placeholder
        let dialect = PostgreSqlDialect {};
        let mut tokenizer = Tokenizer::new(&dialect, sql);
        let tokens = tokenizer.tokenize().unwrap();
        let mut i = 0;
        let tokens: Vec<Token> = tokens
            .iter()
            .map(|t| match t {
                Token::Char(c) if *c == '?' => {
                    i += 1;
                    Token::Word(Word {
                        value: format!("${}", i),
                        quote_style: None,
                        keyword: "".to_owned(),
                    })
                }
                _ => t.clone(),
            })
            .collect();
        let sql = tokens
            .iter()
            .map(|t| format!("{}", t))
            .collect::<Vec<String>>()
            .join("");

        Ok(Rc::new(RefCell::new(PStatement {
            conn: &self.conn,
            sql,
        })) as Rc<RefCell<dyn rdbc::Statement>>)
    }
}

struct PStatement<'a> {
    conn: &'a Connection,
    sql: String,
}

impl<'a> rdbc::Statement for PStatement<'a> {
    fn execute_query(
        &mut self,
        params: &Vec<rdbc::Value>,
    ) -> rdbc::Result<Rc<RefCell<dyn rdbc::ResultSet + '_>>> {
        let params = to_postgres_value(params);
        let params: Vec<&dyn postgres::types::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        self.conn
            .query(&self.sql, params.as_slice())
            .map_err(|e| to_rdbc_err(&e))
            .map(|rows| {
                Rc::new(RefCell::new(PResultSet { i: 0, rows })) as Rc<RefCell<dyn rdbc::ResultSet>>
            })
    }

    fn execute_update(&mut self, params: &Vec<rdbc::Value>) -> rdbc::Result<usize> {
        let params = to_postgres_value(params);
        let params: Vec<&dyn postgres::types::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        self.conn
            .execute(&self.sql, params.as_slice())
            .map_err(|e| to_rdbc_err(&e))
            .map(|n| n as usize)
    }
}

struct PResultSet {
    i: usize,
    rows: Rows,
}

impl rdbc::ResultSet for PResultSet {
    fn next(&mut self) -> bool {
        if self.i < self.rows.len() {
            self.i = self.i + 1;
            true
        } else {
            false
        }
    }

    fn get_i32(&self, i: usize) -> Option<i32> {
        self.rows.get(self.i - 1).get(i - 1)
    }

    fn get_string(&self, i: usize) -> Option<String> {
        self.rows.get(self.i - 1).get(i - 1)
    }
}

fn to_postgres_value(values: &Vec<rdbc::Value>) -> Vec<Box<dyn postgres::types::ToSql>> {
    values
        .iter()
        .map(|v| match v {
            rdbc::Value::String(s) => Box::new(s.clone()) as Box<dyn postgres::types::ToSql>,
            rdbc::Value::Int32(n) => Box::new(*n) as Box<dyn postgres::types::ToSql>,
            rdbc::Value::UInt32(n) => Box::new(*n) as Box<dyn postgres::types::ToSql>,
        })
        .collect()
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn execute_query() -> rdbc::Result<()> {
        execute("DROP TABLE IF EXISTS test", &vec![])?;
        execute("CREATE TABLE test (a INT NOT NULL)", &vec![])?;
        execute(
            "INSERT INTO test (a) VALUES (?)",
            &vec![rdbc::Value::Int32(123)],
        )?;

        let driver = PostgresDriver::new();
        let conn = driver.connect("postgres://rdbc:secret@127.0.0.1:5433")?;
        let mut conn = conn.as_ref().borrow_mut();
        let stmt = conn.prepare("SELECT a FROM test")?;
        let mut stmt = stmt.borrow_mut();
        let rs = stmt.execute_query(&vec![])?;

        let mut rs = rs.as_ref().borrow_mut();

        assert!(rs.next());
        assert_eq!(Some(123), rs.get_i32(1));
        assert!(!rs.next());

        Ok(())
    }

    fn execute(sql: &str, values: &Vec<rdbc::Value>) -> rdbc::Result<usize> {
        println!("Executing '{}' with {} params", sql, values.len());
        let driver = PostgresDriver::new();
        let conn = driver.connect("postgres://rdbc:secret@127.0.0.1:5433")?;
        let mut conn = conn.as_ref().borrow_mut();
        let stmt = conn.prepare(sql)?;
        let mut stmt = stmt.borrow_mut();
        stmt.execute_update(values)
    }
}
