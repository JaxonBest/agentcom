SHELL := /bin/bash
.PHONY: verify

verify:
	@echo "==> Checking README.md line count (<= 400)"
	@lines=$$(wc -l < README.md); \
	if [ "$$lines" -gt 400 ]; then \
		echo "FAIL: README.md has $$lines lines (limit: 400)"; exit 1; \
	else \
		echo "OK: README.md has $$lines lines"; \
	fi

	@echo "==> Checking docs/ links in README.md resolve to real files"
	@fail=0; \
	while IFS= read -r link; do \
		if [ ! -f "$$link" ]; then \
			echo "FAIL: broken link -> $$link"; fail=1; \
		else \
			echo "OK: $$link"; \
		fi; \
	done < <(grep -oE 'docs/[a-zA-Z0-9._/-]+\.md' README.md | sort -u); \
	if [ "$$fail" -ne 0 ]; then exit 1; fi

	@echo "==> All checks passed"
