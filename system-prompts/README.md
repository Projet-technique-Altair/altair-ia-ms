System prompts used by `altair-ia-ms`.

Current files:
- `base.md`: shared system behavior for lab generation/edition runs.
- `ctf-generation/layer1_base_system.txt`: base generation behavior (layer 1).
- `ctf-generation/layer2_form_field_semantics.txt`: form field semantics/rules (layer 2).
- `ctf-generation/layer3_output_contract.txt`: strict output contract (layer 3).
- `ctf-generation/variant_playbook_append.txt`: extra rules applied in variant mode.
- `ctf-generation/playbooks/web_v1.txt`: web playbook.
- `ctf-generation/playbooks/terminal_v1.txt`: terminal playbook.

Notes:
- `src/services/prompts/mod.rs` now loads these files at runtime.
- Legacy `playbooks/*.txt` are still accepted as fallback paths for backward compatibility.

Versioning rule:
- update prompt files with explicit commits;
- keep prompts deterministic and backend-safe;
- backend business rules always override prompt suggestions.
