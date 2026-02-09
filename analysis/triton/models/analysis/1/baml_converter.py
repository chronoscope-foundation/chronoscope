"""Convert JSON Schema to BAML-style type definitions.

BAML syntax is more concise and LLM-friendly than JSON Schema.
This produces a compact representation that preserves field descriptions.
"""

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

    def is_tagged_union(spec: dict[str, Any]) -> bool:
        """Check if a oneOf spec is a tagged union.

        Each variant must be an object with a shared discriminant field.
        """
        return find_tag_field(spec) is not None

    def find_tag_field(spec: dict[str, Any]) -> str | None:
        """Find the discriminant field name in a tagged union."""
        variants = spec.get("oneOf", [])
        if not variants:
            return None

        common_props = set(variants[0].get("properties", {}).keys())
        for v in variants[1:]:
            common_props &= set(v.get("properties", {}).keys())

        for prop in common_props:
            is_tag = True
            for v in variants:
                prop_spec = v["properties"].get(prop, {})
                if not (
                    prop_spec.get("type") == "string"
                    and "enum" in prop_spec
                    and len(prop_spec["enum"]) == 1
                ):
                    is_tag = False
                    break
            if is_tag:
                return str(prop)

        return None

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

    def convert_tagged_union(name: str, spec: dict[str, Any]) -> str:
        """Convert a tagged union (internally-tagged enum) to BAML-style representation.

        Outputs each variant as a separate class-like block showing which fields
        apply for each tag value.
        """
        result = []
        desc = spec.get("description")
        if desc:
            escaped = desc.replace('"', '\\"').replace("\n", " ")
            result.append(f'@description("{escaped}")')

        tag_field = find_tag_field(spec)
        if not tag_field:
            return f"// {name}: unknown tagged union"

        result.append(
            f'// {name}: output ONE of the following (distinguished by "{tag_field}" field):'
        )
        result.append("")

        for variant in spec.get("oneOf", []):
            tag_value = variant["properties"][tag_field]["enum"][0]
            variant_desc = variant.get("description", "")

            if variant_desc:
                escaped = variant_desc.replace('"', '\\"').replace("\n", " ")
                result.append(f'// When {tag_field} = "{tag_value}": {escaped}')
            else:
                result.append(f'// When {tag_field} = "{tag_value}":')

            result.append(f"class {name}_{tag_value} {{")

            properties = variant.get("properties", {})
            required = set(variant.get("required", []))

            for prop_name, prop_spec in properties.items():
                if prop_name == tag_field:
                    # Show the tag field with its fixed value
                    result.append(f'  {prop_name} "{tag_value}" @description("discriminant")')
                    continue

                prop_type = resolve_type(prop_spec)
                if prop_name not in required and not prop_type.endswith("?"):
                    prop_type = f"{prop_type}?"

                prop_desc = prop_spec.get("description")
                if prop_desc:
                    escaped = prop_desc.replace('"', '\\"').replace("\n", " ")
                    result.append(f'  {prop_name} {prop_type} @description("{escaped}")')
                else:
                    result.append(f"  {prop_name} {prop_type}")

            result.append("}")
            result.append("")

        return "\n".join(result)

    # Convert all definitions
    for def_name, def_spec in definitions.items():
        # Detect if it's an enum (has oneOf with all single enum string values)
        if "oneOf" in def_spec:
            variants = def_spec["oneOf"]
            if all("enum" in v and len(v["enum"]) == 1 for v in variants):
                lines.append(convert_enum(def_name, def_spec))
                lines.append("")
                continue

            # Detect tagged union (oneOf with object variants sharing a discriminant)
            if is_tagged_union(def_spec):
                lines.append(convert_tagged_union(def_name, def_spec))
                lines.append("")
                continue

        if "anyOf" in def_spec:
            raise ValueError(
                f"Unsupported: definition '{def_name}' uses anyOf "
                f"(untagged union). All Rust enums should be tagged."
            )

        # Otherwise it's a class/object
        if def_spec.get("type") == "object" or "properties" in def_spec:
            lines.append(convert_class(def_name, def_spec))
            lines.append("")

    # Convert the root type
    if "properties" in schema:
        root_name = schema.get("title", "Output")
        lines.append(convert_class(root_name, schema))
    elif "oneOf" in schema:
        root_name = schema.get("title", "Output")
        if is_tagged_union(schema):
            lines.append(convert_tagged_union(root_name, schema))
        else:
            variants = schema["oneOf"]
            if all("enum" in v and len(v["enum"]) == 1 for v in variants):
                lines.append(convert_enum(root_name, schema))
            else:
                raise ValueError(
                    f"Unsupported: root type '{root_name}' has oneOf "
                    f"that is neither a simple enum nor a tagged union."
                )

    return "\n".join(lines)
