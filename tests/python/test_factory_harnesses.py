import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_shared_factory_resources():
    for harness in ('.pi', '.claude'):
        for item in ('agents', 'skills/factory/SKILL.md'):
            assert (ROOT / harness / item).samefile(ROOT / '.harnesses' / item)


def test_pi_preserves_role_tools_without_model_pins():
    overrides = json.loads((ROOT / '.pi/settings.json').read_text())['subagents']['agentOverrides']
    translation = dict(Read='read', Grep='grep', Glob='find', Bash='bash', Edit='edit', Write='write')
    roles = {p.stem: p for p in (ROOT / '.harnesses/agents').glob('*.md')}
    assert {'intake', 'spec-writer', 'builder', 'approver', 'reviewer-code', 'reviewer-security',
            'reviewer-ux', 'reviewer-ui'} <= roles.keys()
    for role, path in roles.items():
        fields = path.read_text().split('---', 2)[1]
        tools = re.search(r'^tools: (.+)$', fields, re.M)[1]
        expected = {translation[t.strip()] for t in tools.split(',')}
        assert set(overrides[role]['tools']) == expected, role
        assert 'model' not in overrides[role], role
