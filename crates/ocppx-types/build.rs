use case::CaseExt;
use lazy_static::lazy_static;
use serde::Deserialize;
use std::{
    borrow::Cow,
    collections::HashMap,
    env, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};
use thiserror::Error;

fn main() -> Result<()> {
    generate_schemas_for_version(Version::V1_6)?;

    Ok(())
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
enum Error {
    #[error("cannot read schemas directory")]
    SchemasNotFound(io::Error),

    #[error("cannot save the compiled schema")]
    CompiledSchemaCannotBeSaved(io::Error),

    #[error("cannot found a schema file")]
    SchemaNotFound(io::Error),

    #[error("cannot read a particular schema")]
    InvalidSchema(serde_json::Error),

    #[error("schema type not supported: `{ty:?}` in `{schema_path}`")]
    SchemaTypeNotSupported {
        ty: SchemaPropertyType,
        schema_path: PathBuf,
    },

    #[error("schema property type not supported: `{name}: {ty:?}` in `{schema_path}`")]
    SchemaPropertyTypeNotSupported {
        name: String,
        ty: SchemaPropertyType,
        schema_path: PathBuf,
    },

    #[error("schema property format not supported: `{name}` with `{format}` in `{schema_path}`")]
    SchemaPropertyFormatNotSupported {
        name: String,
        format: String,
        schema_path: PathBuf,
    },

    #[error("other unknown error")]
    Other,
}

enum Version {
    V1_6,
}

impl Version {
    fn to_str(&self) -> &'static str {
        match self {
            Self::V1_6 => "v1.6",
        }
    }

    fn to_name(&self) -> &'static str {
        match self {
            Self::V1_6 => "v1_6",
        }
    }
}

fn generate_schemas_for_version(version: Version) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let schemas = fs::read_dir(root.join("schemas").join(version.to_str()))
        .map_err(Error::SchemasNotFound)?
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().expect("Cannot read file type").is_file() => {
                let path = entry.path();

                if let Some(extension) = path.extension() {
                    if extension == "json" {
                        return Some(path);
                    }
                }

                None
            }
            _ => None,
        })
        .map(generate_schema)
        .collect::<Result<Vec<_>>>()?;

    let mut into_file_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    into_file_path.push(format!("{version}.rs", version = version.to_name()));

    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(false)
        .open(into_file_path.clone())
        .map_err(Error::CompiledSchemaCannotBeSaved)?;

    file.write_all(
        format!(
            "use serde::{{Serialize, Deserialize}};\n\n{schemas}",
            schemas = schemas.join("\n\n")
        )
        .as_bytes(),
    )
    .map_err(Error::CompiledSchemaCannotBeSaved)?;

    println!(
        "cargo:rustc-env=OCPPX_TYPES_SCHEMA_{suffix}={value}",
        suffix = version.to_name().to_camel(),
        value = into_file_path.as_path().display(),
    );

    Ok(())
}

#[derive(Deserialize, Debug)]
struct Schema {
    id: String,
    title: String,
    #[serde(rename = "type")]
    ty: SchemaPropertyType,
    properties: SchemaProperties,
}

type SchemaProperties = HashMap<String, SchemaProperty>;

// Source: https://json-schema.org/draft/2020-12/json-schema-validation.html
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SchemaProperty {
    // Validation for Any Instance Type.
    #[serde(rename = "type")]
    ty: SchemaPropertyType,
    r#enum: Option<Vec<String>>,

    // Validation for Strings.
    min_length: Option<u32>,
    max_length: Option<u32>,
    pattern: Option<String>,

    // Validation for Arrays.
    items: Option<Box<SchemaProperty>>,
    max_items: Option<u32>,
    min_items: Option<u32>,
    unique_items: Option<bool>,
    max_contains: Option<u32>,
    min_contains: Option<u32>,

    // Validation for Objects.
    properties: Option<SchemaProperties>,
    additional_properties: Option<bool>,
    max_properties: Option<i32>,
    min_properties: Option<i32>,
    required: Option<Vec<String>>,

    // Vocabularies for Semantic Content
    format: Option<String>,
}

#[derive(Deserialize, Copy, Clone, Debug)]
#[serde(rename_all = "lowercase")]
enum SchemaPropertyType {
    Null,
    Boolean,
    Object,
    Array,
    Number,
    String,
    Integer,
}

fn generate_schema(schema_path: PathBuf) -> Result<String> {
    let schema = fs::read_to_string(&schema_path).map_err(Error::SchemaNotFound)?;
    let schema: Schema = serde_json::from_str(schema.as_str()).map_err(Error::InvalidSchema)?;

    use SchemaPropertyType::*;

    let compiled = match schema.ty {
        Object => compile_object(&schema.title, &schema.properties, &schema_path)?,
        ty => return Err(Error::SchemaTypeNotSupported { ty, schema_path }),
    };

    Ok(compiled)
}

fn compile_object(
    raw_name: &str,
    properties: &SchemaProperties,
    schema_path: &PathBuf,
) -> Result<String> {
    let struct_name = raw_name.to_camel();
    let mut other_objects = HashMap::<String, String>::new();
    let fields = properties
        .iter()
        .map(|(name, property)| {
            compile_property(
                struct_name.as_str(),
                name.as_str(),
                property,
                schema_path,
                &mut other_objects,
            )
        })
        .collect::<Result<Vec<_>>>()?
        .join("\n");

    Ok(format!(
        "{other_objects}#[derive(validator::Validate)]\npub struct {struct_name} {{\n    {fields}\n}}",
        other_objects = if other_objects.is_empty() {
            "".to_string()
        } else {
            let mut s = other_objects.values().map(Clone::clone).collect::<Vec<_>>().join("\n");
            s.push_str("\n\n");

            s
        }
    ))
}

fn compile_enum(enum_name: &str, variants: &[String]) -> Result<String> {
    lazy_static! {
        static ref NOT_ID: regex::Regex = regex::Regex::new("[^A-Za-z0-9]").unwrap();
    }

    Ok(format!(
        "#[derive(Serialize, Deserialize)]\npub enum {enum_name} {{\n    {variants}\n}}",
        variants = variants
            .iter()
            .map(|variant| {
                let v = variant.to_camel();
                let mut v = NOT_ID.replace_all(&v, "");

                if &v != variant {
                    v = Cow::Owned(format!("#[serde(rename = \"{variant}\")] {v}"));
                }

                v.to_mut().push(',');

                v.into_owned()
            })
            .collect::<Vec<_>>()
            .join("\n    ")
    ))
}

fn compile_property(
    struct_name: &str,
    raw_name: &str,
    property: &SchemaProperty,
    schema_path: &PathBuf,
    other_objects: &mut HashMap<String, String>,
) -> Result<String> {
    use SchemaPropertyType::*;

    Ok(format!(
        "{validate}pub r#{name}: {ty},",
        validate = {
            let mut v = [match (&property.min_length, &property.max_length) {
                (None, Some(max)) => Some(format!("#[validate(length(min = 1, max = {max}))]")),
                (Some(min), Some(max)) => {
                    Some(format!("#[validate(length(min = {min}, max = {max}))]"))
                }
                (Some(min), None) => Some(format!("#[validate(length(min = {min})]")),
                (None, None) => None,
            }]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");

            if v.is_empty() {
                "".to_string()
            } else {
                v.push(' ');

                v
            }
        },
        name = raw_name.to_snake(),
        ty = match &property.ty {
            Boolean => "bool".to_string(),

            String => {
                if let Some(format) = &property.format {
                    match format.as_str() {
                        "date-time" => "chrono::DateTime<chrono::offset::Utc>",
                        "uri" => "url::Url",
                        _ => {
                            return Err(Error::SchemaPropertyFormatNotSupported {
                                name: raw_name.to_owned(),
                                format: format.to_string(),
                                schema_path: schema_path.clone(),
                            })
                        }
                    }
                    .to_string()
                } else if let Some(variants) = &property.r#enum {
                    let enum_name = format!(
                        "{prefix}{suffix}",
                        prefix = struct_name,
                        suffix = raw_name.to_camel()
                    );

                    other_objects.insert(
                        enum_name.clone(),
                        compile_enum(enum_name.as_str(), variants)?,
                    );

                    enum_name
                } else {
                    "String".to_string()
                }
            }

            Number | Integer => "i32".to_string(),

            Array => {
                let items = &property.items;

                match items.as_deref() {
                    Some(&SchemaProperty {
                        ty: Object,
                        properties: Some(ref properties),
                        ..
                    }) => {
                        let struct_name = format!(
                            "{prefix}{suffix}",
                            prefix = struct_name,
                            suffix = raw_name.to_camel(),
                        );

                        other_objects.insert(
                            struct_name.clone(),
                            compile_object(struct_name.as_str(), properties, schema_path)?,
                        );

                        format!("Vec<{struct_name}>")
                    }

                    Some(&SchemaProperty { ty: String, .. }) => {
                        format!("Vec<String>")
                    }

                    _ => {
                        return Err(Error::SchemaPropertyTypeNotSupported {
                            name: raw_name.to_owned(),
                            ty: Array,
                            schema_path: schema_path.clone(),
                        });
                    }
                }
            }

            Object => {
                if let Some(properties) = &property.properties {
                    let struct_name = format!(
                        "{prefix}{suffix}",
                        prefix = struct_name,
                        suffix = raw_name.to_camel(),
                    );

                    other_objects.insert(
                        struct_name.clone(),
                        compile_object(struct_name.as_str(), &properties, schema_path)?,
                    );

                    struct_name
                } else {
                    return Err(Error::SchemaPropertyTypeNotSupported {
                        name: raw_name.to_owned(),
                        ty: Object,
                        schema_path: schema_path.clone(),
                    });
                }
            }

            ty =>
                return Err(Error::SchemaPropertyTypeNotSupported {
                    name: raw_name.to_owned(),
                    ty: *ty,
                    schema_path: schema_path.clone()
                }),
        }
    ))
}
