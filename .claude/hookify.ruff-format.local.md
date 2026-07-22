---
name: warn-ruff-format
enabled: true
event: file
action: warn
---

⚠️ **Code may need formatting**

You edited files that might not match the project's ruff formatting standards.

To prevent pre-commit hook failures, run:
```bash
make fmt
```

This reformats code in-place according to ruff rules.
