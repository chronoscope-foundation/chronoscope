"""Convert JSON Schema to BAML-style type definitions.

BAML syntax is more concise and LLM-friendly than JSON Schema.
This produces a compact representation that preserves field descriptions.
"""

import json
from typing import Any


def jsonschema_to_baml(schema: dict[str, Any]) -> str:
    """Convert JSON Schema to BAML-style type definitions."""
    definitions = schema.get("definitions", {})
    lines: list[str] = []

    def resolve_type(type_spec: dict[str, Any]) -> str:
        """Convert a JSON Schema type spec to BAML type string."""
        # Handle $ref
        if "$ref" in type_spec:
            ref: str = type_spec["$ref"]
            # "#/definitions/Foo" -> "Foo"
            return ref.split("/")[-1]

        # Handle allOf with single $ref (schemars pattern)
        if "allOf" in type_spec and len(type_spec["allOf"]) == 1:
            return resolve_type(type_spec["allOf"][0])

        # Handle oneOf (enum pattern from schemars)
        if "oneOf" in type_spec:
            # Check if it's an enum (each variant has single enum value)
            variants = type_spec["oneOf"]
            if all("enum" in v and len(v["enum"]) == 1 for v in variants):
                # This is an inline enum, but usually these are $ref'd
                return "string"
            return "any"

        # Handle nullable types: type: ["string", "null"]
        if isinstance(type_spec.get("type"), list):
            types = [t for t in type_spec["type"] if t != "null"]
            if len(types) == 1:
                base = type_to_baml(types[0])
                return f"{base}?"
            return "any"

        # Handle arrays
        if type_spec.get("type") == "array":
            items = type_spec.get("items", {})
            item_type = resolve_type(items)
            return f"{item_type}[]"

        # Handle objects with additionalProperties (maps)
        if type_spec.get("type") == "object" and "additionalProperties" in type_spec:
            value_type = resolve_type(type_spec["additionalProperties"])
            return f"map<string, {value_type}>"

        # Handle basic types
        return type_to_baml(type_spec.get("type", "any"))

    def type_to_baml(json_type: str | None) -> str:
        """Convert JSON Schema type to BAML type."""
        return {
            "string": "string",
            "integer": "int",
            "number": "float",
            "boolean": "bool",
            "object": "object",
            "array": "array",
            None: "any",
        }.get(json_type, "any")

    def convert_enum(name: str, spec: dict[str, Any]) -> str:
        """Convert a schemars enum to BAML enum."""
        result = []
        desc = spec.get("description")
        if desc:
            escaped = desc.replace('"', '\\"').replace("\n", " ")
            result.append(f'@description("{escaped}")')
        result.append(f"enum {name} {{")

        for variant in spec.get("oneOf", []):
            if "enum" in variant and len(variant["enum"]) == 1:
                variant_name = variant["enum"][0]
                variant_desc = variant.get("description")
                if variant_desc:
                    escaped = variant_desc.replace('"', '\\"').replace("\n", " ")
                    result.append(f'  {variant_name} @description("{escaped}")')
                else:
                    result.append(f"  {variant_name}")

        result.append("}")
        return "\n".join(result)

    def convert_union(name: str, spec: dict[str, Any]) -> str:
        """Convert an anyOf (untagged union) to BAML-style documentation.

        Since BAML doesn't have native union syntax, we document the variants.
        """
        result = []
        desc = spec.get("description")
        if desc:
            # Extract first sentence for the main description
            first_sentence = desc.split(".")[0] + "."
            escaped = first_sentence.replace('"', '\\"').replace("\n", " ")
            result.append(f'@description("{escaped}")')

        result.append(f"// {name} is ONE OF the following:")

        for i, variant in enumerate(spec.get("anyOf", []), 1):
            variant_desc = variant.get("description", "")

            # Check if it's a $ref or allOf with $ref
            if "$ref" in variant:
                ref_name = variant["$ref"].split("/")[-1]
                result.append(f"//   {i}. A {ref_name} object")
            elif "allOf" in variant and len(variant["allOf"]) == 1:
                ref_name = resolve_type(variant["allOf"][0])
                if variant_desc:
                    result.append(f"//   {i}. A {ref_name} object - {variant_desc}")
                else:
                    result.append(f"//   {i}. A {ref_name} object")
            elif "properties" in variant:
                # Inline object - describe its structure
                props = list(variant.get("properties", {}).keys())
                if variant_desc:
                    result.append(f"//   {i}. An object with: {', '.join(props)} - {variant_desc}")
                else:
                    result.append(f"//   {i}. An object with: {', '.join(props)}")

        return "\n".join(result)

    def convert_class(name: str, spec: dict[str, Any]) -> str:
        """Convert a JSON Schema object to BAML class."""
        result = []
        desc = spec.get("description")
        if desc:
            escaped = desc.replace('"', '\\"').replace("\n", " ")
            result.append(f'@description("{escaped}")')
        result.append(f"class {name} {{")

        properties = spec.get("properties", {})
        required = set(spec.get("required", []))

        for prop_name, prop_spec in properties.items():
            prop_type = resolve_type(prop_spec)
            # Mark optional fields
            if prop_name not in required and not prop_type.endswith("?"):
                prop_type = f"{prop_type}?"

            prop_desc = prop_spec.get("description")
            if prop_desc:
                escaped = prop_desc.replace('"', '\\"').replace("\n", " ")
                result.append(f'  {prop_name} {prop_type} @description("{escaped}")')
            else:
                result.append(f"  {prop_name} {prop_type}")

        result.append("}")
        return "\n".join(result)

    # Convert all definitions
    for def_name, def_spec in definitions.items():
        # Detect if it's an enum (has oneOf with enum variants)
        if "oneOf" in def_spec:
            variants = def_spec["oneOf"]
            if all("enum" in v and len(v["enum"]) == 1 for v in variants):
                lines.append(convert_enum(def_name, def_spec))
                lines.append("")
                continue

        # Detect if it's a union type (anyOf)
        if "anyOf" in def_spec:
            lines.append(convert_union(def_name, def_spec))
            lines.append("")
            continue

        # Otherwise it's a class/object
        if def_spec.get("type") == "object" or "properties" in def_spec:
            lines.append(convert_class(def_name, def_spec))
            lines.append("")

    # Convert the root type (usually the main output type)
    if "properties" in schema:
        root_name = schema.get("title", "Output")
        lines.append(convert_class(root_name, schema))

    return "\n".join(lines)
