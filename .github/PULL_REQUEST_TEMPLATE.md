# Pull Request Template

## Description

Summary of the change and the issue it fixes, with any context or dependencies.
Fixes # (issue)

## Type of change

- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature or newly ported API area (non-breaking change which adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality not to work as expected)
- [ ] This change requires a documentation update

## How has this been tested?

Commands run and what they showed (`cargo test`, `cargo clippy`, manual check in the dashboard).

## Checklist

- [ ] Branch is `NRR-<number>/<type>-<description>`, cut from `develop`
- [ ] PR title follows Conventional Commits (`feat: ...`, `fix: ...`)
- [ ] No AI co-author lines in commits (AI use may be disclosed here instead)
- [ ] New `/v1` and `/api` handlers enforce API-key / dashboard auth
- [ ] I have performed a self-review of my code
- [ ] I have updated the documentation where needed
- [ ] New and existing tests pass locally
