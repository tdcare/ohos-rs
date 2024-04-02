use crate::build::Context;
use crate::create_project_file;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::{Arc, RwLock};

const TOP_LEVEL_NAMESPACE: &str = "__TOP_LEVEL_MODULE__";
const DEFAULT_TYPE_DEF_HEADER: &str = "/* auto-generated by NAPI-RS-OHOS */
/* eslint-disable */
";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum TypeDefKind {
  Const,
  Enum,
  Interface,
  Fn,
  Struct,
  Impl,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct TypeDefLine {
  kind: TypeDefKind,
  name: String,
  original_name: Option<String>,
  def: String,
  js_doc: Option<String>,
  js_mod: Option<String>,
}

fn read_intermediate_type_file(file_path: &str) -> Vec<TypeDefLine> {
  let path = Path::new(file_path);
  let file = File::open(path).unwrap();
  let lines = io::BufReader::new(file).lines();

  let mut defs: Vec<TypeDefLine> = Vec::new();
  for line in lines {
    if let Ok(json_line) = line {
      let mut format_line = json_line.trim().to_string();

      // 检查字符串是否以'{'开头
      if !format_line.starts_with('{') {
        // 找到第一个':'的位置
        if let Some(start) = format_line.find(':') {
          // 从':'的下一个位置开始切片字符串
          format_line = format_line[start + 1..].to_string();
        }
      }
      if !format_line.is_empty() {
        let json_value: TypeDefLine = serde_json::from_str(&format_line).unwrap();
        defs.push(json_value);
      }
    }
  }

  defs.sort_unstable_by(|a, b| match (a.kind, b.kind) {
    (TypeDefKind::Struct, TypeDefKind::Struct) => a.name.cmp(&b.name),
    (TypeDefKind::Struct, _) => std::cmp::Ordering::Less,
    (_, TypeDefKind::Struct) => std::cmp::Ordering::Greater,
    _ => a.name.cmp(&b.name),
  });
  defs
}

// The process_type_def function to process type definitions
fn process_type_def(
  intermediate_type_file: &str,
  const_enum: bool,
  header: &str,
) -> (String, Vec<String>) {
  let mut exports: Vec<String> = Vec::new();
  let defs = read_intermediate_type_file(intermediate_type_file);
  let mut grouped_defs = preprocess_type_def(defs);

  let mut header = String::from(header);
  let mut dts = String::new();

  // Sort and process the definitions
  let mut sorted_grouped_defs = grouped_defs.drain().collect::<Vec<_>>();
  sorted_grouped_defs.sort_by_key(|(namespace, _)| namespace.clone());

  for (namespace, defs) in sorted_grouped_defs {
    if namespace == TOP_LEVEL_NAMESPACE {
      for def in defs {
        dts += &pretty_print(&def, const_enum, 0);
        dts.push('\n');
        dts.push('\n');
        match def.kind {
          TypeDefKind::Const | TypeDefKind::Enum | TypeDefKind::Fn | TypeDefKind::Struct => {
            exports.push(def.name.clone());
            if let Some(original_name) = def.original_name {
              if original_name != def.name {
                exports.push(original_name);
              }
            }
          }
          _ => {}
        }
      }
    } else {
      exports.push(namespace.clone());
      dts += &format!("export namespace {} {{\n", namespace);
      for def in defs {
        dts += &pretty_print(&def, const_enum, 2);
        dts.push('\n');
      }
      dts.push_str("}\n\n");
    }
  }

  let mut has_import = false;

  let buffer_reg = Regex::new(r"\bBuffer\b").unwrap();
  if buffer_reg.is_match(&dts) {
    has_import = true;
    dts = buffer_reg.replace_all(&dts, "buffer.Buffer").to_string();
    header.push_str("import buffer from '@ohos.buffer';\n");
  }

  let abort_reg = Regex::new(r"\bAbortSignal\b").unwrap();
  if abort_reg.is_match(&dts) {
    has_import = true;
    header.push_str(super::abort_tmp::ABORT_TS);
    println!("OHOS-RS:::: For AbortController, you can use @ohos-rs/abort-controller.");
  }

  if has_import {
    header.push_str("\n\n");
  }

  if dts.contains("ExternalObject<") {
    header.push_str(
      r#"
export class ExternalObject<T> {
  readonly '': {
    readonly '': unique symbol
    [K: symbol]: T
  }
}
"#,
    );
  }

  dts.insert_str(0, header.as_str());

  (dts, exports)
}

// Helper function to preprocess type definitions
fn preprocess_type_def(defs: Vec<TypeDefLine>) -> HashMap<String, Vec<TypeDefLine>> {
  let mut namespace_grouped: HashMap<String, Vec<TypeDefLine>> = HashMap::new();
  let mut class_defs: HashMap<String, TypeDefLine> = HashMap::new();

  for def in defs {
    let namespace = def
      .js_mod
      .clone()
      .unwrap_or_else(|| TOP_LEVEL_NAMESPACE.to_string());
    namespace_grouped
      .entry(namespace.clone())
      .or_insert_with(Vec::new);

    let group = namespace_grouped.get_mut(&namespace).unwrap();

    match def.kind {
      TypeDefKind::Struct => {
        group.push(def.clone());
        class_defs.insert(def.name.clone(), def);
      }
      TypeDefKind::Impl => {
        if let Some(class_def) = class_defs.get_mut(&def.name) {
          class_def.def.push('\n');
          class_def.def += &def.def;
        }
      }
      _ => {
        group.push(def);
      }
    }
  }

  namespace_grouped
}

// Helper function to format the string with the correct indentation
fn pretty_print(line: &TypeDefLine, const_enum: bool, indent: usize) -> String {
  let mut s = line.js_doc.clone().unwrap_or_default();
  match line.kind {
    TypeDefKind::Interface => {
      s += &format!("export interface {} {{\n{}\n}}", line.name, line.def);
    }
    TypeDefKind::Enum => {
      let enum_name = if const_enum { "const enum" } else { "enum" };
      s += &format!("export {} {} {{\n{}\n}}", enum_name, line.name, line.def);
    }
    TypeDefKind::Struct => {
      s += &format!("export class {} {{\n{}\n}}", line.name, line.def);
      if let Some(original_name) = &line.original_name {
        if original_name != &line.name {
          s += &format!("\nexport type {} = {}", original_name, line.name);
        }
      }
    }
    _ => {
      s += &line.def;
    }
  }

  correct_string_indent(&s, indent)
}

fn correct_string_indent(src: &str, indent: usize) -> String {
  let mut result = String::new();
  let mut bracket_depth = 0;
  for line in src.lines() {
    let line = line.trim();
    if line.is_empty() {
      result.push('\n');
      continue;
    }

    let is_in_multiline_comment = line.starts_with('*');
    let is_closing_bracket = line.ends_with('}');
    let is_opening_bracket = line.ends_with('{');

    let right_indent = if is_opening_bracket && !is_in_multiline_comment {
      bracket_depth += 1;
      indent + (bracket_depth - 1) * 2
    } else {
      if is_closing_bracket && bracket_depth > 0 && !is_in_multiline_comment {
        bracket_depth -= 1;
      }
      indent + bracket_depth * 2
    };

    let indented_line = if is_in_multiline_comment {
      format!("{} {}", " ".repeat(right_indent + 1), line)
    } else {
      format!("{}{}", " ".repeat(right_indent), line)
    };

    result.push_str(&indented_line);
    result.push('\n');
  }

  result
}
pub fn generate_d_ts_file(c: Arc<RwLock<Context>>) {
  let ctx = c.read().unwrap();
  let tmp_file = env::var("TYPE_DEF_TMP_PATH").unwrap();
  let (dts, _exports) = process_type_def(&tmp_file, true, DEFAULT_TYPE_DEF_HEADER);
  let dest_file_path = ctx.dist.join("index.d.ts");
  create_project_file!(dts, dest_file_path, "index.d.ts");
}
