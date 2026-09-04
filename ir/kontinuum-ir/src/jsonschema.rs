//! Hand-written, deliberately coarse JSON Schema for the IR (issue #11).
//!
//! `schemars` is unavailable offline, so this schema is authored by hand and
//! covers the contract a guided generator actually needs: required fields,
//! types, enums, and the documented numeric bounds. It is intentionally not a
//! full mirror of `deny_unknown_fields` edge semantics; `validate_session`
//! remains the authoritative gate.

use serde_json::{json, Value};

/// Coarse JSON Schema (draft 2020-12) for the session document.
pub fn export_json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://kontinuum.dev/schemas/ir-v1.json",
        "title": "Kontinuum IR session",
        "description": "Coarse schema; validate_session is the authoritative gate. Hand-written (schemars unavailable offline).",
        "type": "object",
        "additionalProperties": false,
        "required": ["version", "seed", "tempo_lane", "sections", "tracks"],
        "properties": {
            "version": { "const": 1 },
            "seed": { "type": "integer", "minimum": 0 },
            "tempo_lane": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "array",
                    "prefixItems": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "number", "exclusiveMinimum": 0, "maximum": 1000 }
                    ],
                    "minItems": 2, "maxItems": 2
                }
            },
            "key": { "type": ["string", "null"] },
            "palette": {},
            "duck_release_ms": { "type": "number", "minimum": 20, "maximum": 1000 },
            "pattern_engine": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "groove": { "type": "string" },
                    "swing": { "type": "number", "minimum": 0, "maximum": 0.5 },
                    "bias_ticks": { "type": "integer", "minimum": -12, "maximum": 12 },
                    "jitter_ticks": { "type": "number", "minimum": 1, "maximum": 4 },
                    "bass_archetype": { "type": "string" },
                    "downbeat_collision": { "enum": ["avoid", "allow", "duck_only"] }
                }
            },
            "souls": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "weight"],
                    "properties": {
                        "id": { "type": "string", "minLength": 1 },
                        "weight": { "type": "number", "exclusiveMinimum": 0, "maximum": 1 },
                        "era": { "type": ["string", "null"], "minLength": 1 }
                    }
                }
            },
            "sections": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/section" } },
            "tracks": { "type": "array", "minItems": 1, "maxItems": 255, "items": { "$ref": "#/$defs/track" } }
        },
        "$defs": {
            "section": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "bars"],
                "properties": {
                    "id": { "type": "string", "minLength": 1 },
                    "bars": { "type": "integer", "minimum": 1 },
                    "energy_curve": { "type": "array", "minItems": 1, "items": { "type": "number", "minimum": 0, "maximum": 1 } },
                    "density_curve": { "type": "array", "minItems": 1, "items": { "type": "number", "minimum": 0, "maximum": 1 } },
                    "brightness_curve": { "type": "array", "minItems": 1, "items": { "type": "number", "minimum": 0, "maximum": 1 } },
                    "transition_in": { "$ref": "#/$defs/transition" },
                    "transition_out": { "$ref": "#/$defs/transition" },
                    "pattern_bindings": {
                        "type": "object",
                        "additionalProperties": { "$ref": "#/$defs/pattern" }
                    },
                    "automation": {
                        "type": "object",
                        "additionalProperties": { "$ref": "#/$defs/automation_lane" }
                    }
                }
            },
            "transition": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "bars"],
                "properties": {
                    "type": { "enum": ["filter_sweep", "mute_choreo", "fill", "silence_drop", "riser", "reverb_throw"] },
                    "bars": { "type": "integer", "minimum": 1 },
                    "params": {}
                }
            },
            "automation_lane": {
                "type": "object",
                "additionalProperties": false,
                "required": ["target_param"],
                "properties": {
                    "target_param": { "enum": ["gain", "pan", "insert0", "insert1", "send_delay", "send_reverb"] },
                    "points": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "array", "minItems": 3, "maxItems": 3,
                            "prefixItems": [
                                { "type": "integer", "minimum": 0 },
                                { "type": "number" },
                                { "enum": ["linear", "exp", "smooth"] }
                            ]
                        }
                    }
                }
            },
            "track": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "role", "instrument"],
                "properties": {
                    "id": { "type": "string", "minLength": 1 },
                    "role": { "enum": ["kick", "bass", "perc", "pad", "fx"] },
                    "instrument": { "$ref": "#/$defs/instrument" },
                    "inserts": { "type": "array", "maxItems": 2, "items": { "$ref": "#/$defs/insert" } },
                    "sends": {
                        "type": "object", "additionalProperties": false,
                        "properties": {
                            "delay": { "type": "number", "minimum": 0, "maximum": 1 },
                            "reverb": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    "gain": { "type": "number", "minimum": 0, "maximum": 2 },
                    "pan": { "type": "number", "minimum": -1, "maximum": 1 },
                    "duck_depth": { "type": ["number", "null"], "minimum": 0, "maximum": 1 }
                }
            },
            "insert": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type"],
                "properties": {
                    "type": { "enum": ["filter", "drive", "delay", "reverb", "chorus", "compressor"] },
                    "params": {},
                    "mix": { "type": "number", "minimum": 0, "maximum": 1 }
                }
            },
            "pattern": {
                "oneOf": [
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["steps"],
                        "properties": {
                            "steps": { "type": "array", "items": { "$ref": "#/$defs/step" } },
                            "repeats": { "type": "integer", "minimum": 1, "maximum": 64 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["generator", "k", "n"],
                        "properties": {
                            "generator": { "const": "euclidean" },
                            "k": { "type": "integer", "minimum": 1 },
                            "n": { "type": "integer", "minimum": 1, "maximum": 4096 },
                            "rot": { "type": "integer" },
                            "velocity": { "type": "number", "minimum": 0, "maximum": 1 },
                            "probability": { "type": "number", "minimum": 0, "maximum": 1 },
                            "repeats": { "type": "integer", "minimum": 1, "maximum": 64 },
                            "gate": { "type": ["number", "null"], "minimum": 0.01, "maximum": 64 },
                            "pitch": { "type": ["number", "null"] }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["generator", "density"],
                        "properties": {
                            "generator": { "const": "probability_mask" },
                            "density": { "type": "number", "minimum": 0, "maximum": 1 },
                            "velocity": { "type": "number", "minimum": 0, "maximum": 1 },
                            "probability": { "type": "number", "minimum": 0, "maximum": 1 },
                            "repeats": { "type": "integer", "minimum": 1, "maximum": 64 },
                            "gate": { "type": ["number", "null"], "minimum": 0.01, "maximum": 64 },
                            "pitch": { "type": ["number", "null"] }
                        }
                    }
                ]
            },
            "step": {
                "type": "object", "additionalProperties": false,
                "required": ["position"],
                "properties": {
                    "position": { "type": "integer", "minimum": 0, "maximum": 3839 },
                    "velocity": { "type": "number", "minimum": 0, "maximum": 1 },
                    "probability": { "type": "number", "minimum": 0, "maximum": 1 },
                    "microtiming_ticks": { "type": "integer", "minimum": -120, "maximum": 120 },
                    "ratchet": { "type": "integer", "minimum": 1, "maximum": 8 },
                    "pitch": { "type": ["number", "null"] },
                    "gate": { "type": ["number", "null"], "minimum": 0.01, "maximum": 64 },
                    "accent": { "type": "boolean", "default": false }
                }
            },
            "instrument": {
                "oneOf": [
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "kick" },
                            "tune_hz": { "type": "number", "minimum": 30, "maximum": 120 },
                            "decay_ms": { "type": "number", "minimum": 50, "maximum": 1500 },
                            "click": { "type": "number", "minimum": 0, "maximum": 1 },
                            "drive": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "hat" },
                            "decay_ms": { "type": "number", "minimum": 5, "maximum": 2000 },
                            "tone": { "type": "number", "minimum": 0, "maximum": 1 },
                            "open": { "type": "boolean" }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "bass" },
                            "cutoff_hz": { "type": "number", "minimum": 40, "maximum": 8000 },
                            "resonance": { "type": "number", "minimum": 0, "maximum": 1 },
                            "wave": { "enum": ["saw", "square"] },
                            "glide_ms": { "type": "number", "minimum": 0, "maximum": 1000 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "pad" },
                            "attack_ms": { "type": "number", "minimum": 1, "maximum": 10000 },
                            "release_ms": { "type": "number", "minimum": 10, "maximum": 20000 },
                            "detune_cents": { "type": "number", "minimum": -100, "maximum": 100 },
                            "cutoff_hz": { "type": "number", "minimum": 40, "maximum": 16000 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "sample" },
                            "query": { "type": ["string", "null"], "minLength": 1 },
                            "id": { "type": ["integer", "null"], "minimum": 0 },
                            "recipe_hash": { "type": ["integer", "null"], "minimum": 0 },
                            "transpose": { "type": "number", "minimum": -36, "maximum": 36 },
                            "fine": { "type": "number", "minimum": -100, "maximum": 100 },
                            "stretch": { "type": "number", "minimum": 0.25, "maximum": 4 },
                            "choke_group": { "type": "integer", "minimum": 1, "maximum": 16 },
                            "granular": { "$ref": "#/$defs/granular_slot" }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "wavetable" },
                            "position": { "type": "number", "minimum": 0, "maximum": 1 },
                            "detune_cents": { "type": "number", "minimum": 0, "maximum": 50 },
                            "osc2_level": { "type": "number", "minimum": 0, "maximum": 1 },
                            "sub": { "type": "number", "minimum": 0, "maximum": 1 },
                            "cutoff_hz": { "type": "number", "minimum": 100, "maximum": 12000 },
                            "release_ms": { "type": "number", "minimum": 20, "maximum": 8000 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "fmperc" },
                            "ratio": { "type": "number", "minimum": 0.25, "maximum": 8 },
                            "index": { "type": "number", "minimum": 0, "maximum": 8 },
                            "feedback": { "type": "number", "minimum": 0, "maximum": 1 },
                            "decay_ms": { "type": "number", "minimum": 20, "maximum": 3000 },
                            "preset": { "enum": ["metallic", "tom", "bell"] }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind"],
                        "properties": {
                            "kind": { "const": "texture" },
                            "crackle": { "type": "boolean" },
                            "density": { "type": "number", "minimum": 0, "maximum": 0.05 },
                            "grain_ms": { "type": "number", "minimum": 2, "maximum": 200 },
                            "tone": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["kind", "patch"],
                        "properties": {
                            "kind": { "const": "custom" },
                            "patch": { "$ref": "#/$defs/patch" }
                        }
                    }
                ]
            },
            "patch": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "nodes": { "type": "array", "maxItems": 24, "items": { "$ref": "#/$defs/patch_node" } },
                    "edges": { "type": "array", "maxItems": 32, "items": { "$ref": "#/$defs/patch_edge" } }
                }
            },
            "granular_slot": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "grain_ms": { "type": "number", "minimum": 20, "maximum": 200 },
                    "density": { "type": "number", "minimum": 1, "maximum": 200 },
                    "spray_ms": { "type": "number", "minimum": 0, "maximum": 1000 },
                    "pitch_jitter_cents": { "type": "number", "minimum": 0, "maximum": 1200 },
                    "level": { "type": "number", "minimum": 0, "maximum": 1 }
                }
            },
            "patch_node": {
                "oneOf": [
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "osc" },
                            "wave": { "enum": ["saw", "square", "sine", "tri", "noise"] },
                            "unison": { "type": "integer", "minimum": 1, "maximum": 7 },
                            "fine_cents": { "type": "number", "minimum": -100, "maximum": 100 },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "fm_pair" },
                            "ratio": { "type": "number", "minimum": 0.25, "maximum": 16 },
                            "index": { "type": "number", "minimum": 0, "maximum": 8 },
                            "feedback": { "type": "number", "minimum": 0, "maximum": 1 },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "filter" },
                            "mode": { "enum": ["low_pass", "high_pass", "band_pass"] },
                            "cutoff_hz": { "type": "number", "minimum": 20, "maximum": 20000 },
                            "resonance": { "type": "number", "minimum": 0, "maximum": 1 },
                            "drive": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "env" },
                            "attack_ms": { "type": "number", "minimum": 1, "maximum": 10000 },
                            "decay_ms": { "type": "number", "minimum": 1, "maximum": 10000 },
                            "sustain": { "type": "number", "minimum": 0, "maximum": 1 },
                            "release_ms": { "type": "number", "minimum": 10, "maximum": 20000 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "lfo" },
                            "rate_hz": { "type": "number", "minimum": 0.01, "maximum": 40 },
                            "depth": { "type": "number", "minimum": 0, "maximum": 1 },
                            "wave": { "enum": ["sine", "tri", "square"] }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "gain" },
                            "level": { "type": "number", "minimum": 0, "maximum": 2 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "delay" },
                            "time_ms": { "type": "number", "minimum": 1, "maximum": 2000 },
                            "feedback": { "type": "number", "minimum": 0, "maximum": 0.95 },
                            "mix": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "ring" },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "shaper" },
                            "drive": { "type": "number", "minimum": 0, "maximum": 1 },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "formant" },
                            "vowel": { "enum": ["ah", "eh", "ee", "oh", "oo"] },
                            "shift": { "type": "number", "minimum": 0.5, "maximum": 2 },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type", "slot"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "sampler" },
                            "slot": { "type": "integer", "minimum": 0 },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false,
                        "required": ["id", "type"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 },
                            "type": { "const": "out" },
                            "level": { "type": "number", "minimum": 0, "maximum": 1 }
                        }
                    }
                ]
            },
            "patch_edge": {
                "type": "object", "additionalProperties": false,
                "required": ["from", "to", "type"],
                "properties": {
                    "from": { "type": "string", "minLength": 1 },
                    "to": { "type": "string", "minLength": 1 },
                    "type": { "enum": ["audio", "mod"] },
                    "param": { "type": ["string", "null"], "minLength": 1 },
                    "amount": { "type": "number", "minimum": 0, "maximum": 2 }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_object_with_defs() {
        let s = export_json_schema();
        assert_eq!(s["type"], "object");
        assert!(s.get("$defs").is_some());
        assert!(serde_json::to_string(&s).is_ok());
    }
}
