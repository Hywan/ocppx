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
    SchemaNotFound {
        error: io::Error,
        schema_path: PathBuf,
    },

    #[error("cannot read a particular schema")]
    InvalidSchema {
        error: serde_json::Error,
        schema_path: PathBuf,
    },

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

    let mut compiled_schemas = HashMap::<String, String>::new();

    for schema in fs::read_dir(root.join("schemas").join(version.to_str()))
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
    {
        generate_schema(schema, &mut compiled_schemas)?;
    }

    let mut into_file_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    into_file_path.push(format!("{version}.rs", version = version.to_name()));

    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .read(false)
        .open(into_file_path.clone())
        .map_err(Error::CompiledSchemaCannotBeSaved)?;

    file.write_all(
        format!(
            "use serde::{{Serialize, Deserialize}};\n\n{schemas}",
            schemas = compiled_schemas
                .values()
                .map(Clone::clone)
                .collect::<Vec<_>>()
                .join("\n\n"),
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
    required: Option<Vec<String>>,
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

fn generate_schema(
    schema_path: PathBuf,
    compiled_schemas: &mut HashMap<String, String>,
) -> Result<()> {
    let schema = fs::read_to_string(&schema_path).map_err(|error| Error::SchemaNotFound {
        error,
        schema_path: schema_path.clone(),
    })?;
    let schema: Schema =
        serde_json::from_str(schema.as_str()).map_err(|error| Error::InvalidSchema {
            error,
            schema_path: schema_path.clone(),
        })?;

    use SchemaPropertyType::*;

    match schema.ty {
        Object => compile_object(
            &schema.title,
            &schema.properties,
            if let Some(required) = &schema.required {
                required
            } else {
                &[]
            },
            &schema_path,
            compiled_schemas,
        )?,
        ty => return Err(Error::SchemaTypeNotSupported { ty, schema_path }),
    }

    Ok(())
}

fn compile_object(
    raw_name: &str,
    properties: &SchemaProperties,
    required: &[String],
    schema_path: &PathBuf,
    compiled_schemas: &mut HashMap<String, String>,
) -> Result<()> {
    let struct_name = raw_name.to_camel();
    let fields = properties
        .iter()
        .map(|(raw_name, property)| {
            let (annotations, name, ty) = compile_property(
                struct_name.as_str(),
                raw_name.as_str(),
                property,
                schema_path,
                compiled_schemas,
            )?;

            if required.contains(raw_name) {
                Ok(format!("{annotations}pub r#{name}: {ty},"))
            } else {
                Ok(format!("{annotations}pub r#{name}: Option<{ty}>,"))
            }
        })
        .collect::<Result<Vec<_>>>()?
        .join("\n");

    compiled_schemas.insert(
        struct_name.clone(),
        format!("#[derive(Debug, Clone, Serialize, Deserialize, validator::Validate)]\npub struct {struct_name} {{\n    {fields}\n}}",),
    );

    Ok(())
}

fn compile_enum(
    enum_name: &str,
    variants: &[String],
    compiled_schemas: &mut HashMap<String, String>,
) -> Result<()> {
    lazy_static! {
        static ref NOT_ID: regex::Regex = regex::Regex::new("[^A-Za-z0-9]").unwrap();
    }

    compiled_schemas.insert(
        enum_name.to_string(),
        format!(
            "#[derive(Debug, Copy, Clone, Serialize, Deserialize)]\npub enum {enum_name} {{\n    {variants}\n}}",
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
        ),
    );

    Ok(())
}

fn compile_property(
    struct_name: &str,
    raw_name: &str,
    property: &SchemaProperty,
    schema_path: &PathBuf,
    compiled_schemas: &mut HashMap<String, String>,
) -> Result<(String, String, String)> {
    use SchemaPropertyType::*;

    Ok((
        {
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
        raw_name.to_snake(),
        match &property.ty {
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
                    let enum_name = raw_name.to_camel();

                    compile_enum(enum_name.as_str(), variants, compiled_schemas)?;

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
                        ref required,
                        ..
                    }) => {
                        let struct_name = raw_name.to_camel();

                        compile_object(
                            struct_name.as_str(),
                            properties,
                            if let Some(required) = required {
                                required
                            } else {
                                &[]
                            },
                            schema_path,
                            compiled_schemas,
                        )?;

                        format!("Vec<{struct_name}>")
                    }

                    Some(&SchemaProperty { ty: String, .. }) => "Vec<String>".to_string(),

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
                    let struct_name = raw_name.to_camel();

                    compile_object(
                        struct_name.as_str(),
                        properties,
                        if let Some(required) = &property.required {
                            required
                        } else {
                            &[]
                        },
                        schema_path,
                        compiled_schemas,
                    )?;

                    struct_name
                } else {
                    return Err(Error::SchemaPropertyTypeNotSupported {
                        name: raw_name.to_owned(),
                        ty: Object,
                        schema_path: schema_path.clone(),
                    });
                }
            }

            ty => {
                return Err(Error::SchemaPropertyTypeNotSupported {
                    name: raw_name.to_owned(),
                    ty: *ty,
                    schema_path: schema_path.clone(),
                })
            }
        },
    ))
}
