import json
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
FACTORY = ROOT / 'factory'


def call(factory, *args):
    return subprocess.run([factory / 'bin/route', *args], cwd=factory.parent,
                          capture_output=True, text=True, timeout=5)


@pytest.fixture
def isolated(tmp_path):
    factory = tmp_path / 'factory'
    (factory / 'bin').mkdir(parents=True)
    shutil.copy2(FACTORY / 'bin/route', factory / 'bin/route')
    shutil.copy2(FACTORY / 'config.json', factory / 'config.json')
    return factory


@pytest.mark.parametrize('alias', ['fable', 'opus', 'sonnet', 'haiku'])
def test_lookup_without_job(isolated, alias):
    expected = json.loads((isolated / 'config.json').read_text())['pi_model_map'][alias]
    assert re.fullmatch(r'openai-codex/[^/\s]+', expected)
    result = call(isolated, '--pi-model', alias)
    assert (result.returncode, result.stdout, result.stderr) == (0, expected + '\n', '')
    assert not (isolated / 'jobs').exists()


def test_reads_updated_pin(isolated):
    (isolated / 'config.json').write_text(json.dumps({'pi_model_map': {'opus': 'test/changed'}}))
    result = call(isolated, '--pi-model', 'opus')
    assert (result.returncode, result.stdout, result.stderr) == (0, 'test/changed\n', '')


@pytest.mark.parametrize('table', [None, {}, {'new': None}, {'new': 'terra'}, {'new': 'x/'},
                                   {'new': '/y'}, {'new': 'x/y z'}, {'new': 'x/y/'}])
def test_bad_mapping_does_not_print_a_model(isolated, table):
    (isolated / 'config.json').write_text(json.dumps({'pi_model_map': table}))
    result = call(isolated, '--pi-model', 'new')
    assert result.returncode != 0 and result.stdout == ''
    assert 'new' in result.stderr and 'pi_model_map' in result.stderr


def test_covers_every_configured_tier():
    config = json.loads((FACTORY / 'config.json').read_text())
    tiers = {v for section in ('routing', 'surface_reviewers')
             for row in config[section].values() for v in row.values()}
    for agent in (ROOT / '.harnesses/agents').glob('*.md'):
        frontmatter = agent.read_text().split('---', 2)[1]
        tiers.add(re.search(r'^model: (\S+)$', frontmatter, re.M)[1])
    assert tiers <= config['pi_model_map'].keys()


@pytest.mark.parametrize(('risk', 'code'), [('low', 'haiku'), ('high', 'sonnet')])
def test_existing_route_stores_aliases(isolated, risk, code):
    folder = isolated / 'jobs/fixture'
    folder.mkdir(parents=True)
    (folder / 'job.json').write_text(json.dumps({'intake': {'risk': risk, 'surfaces': ['security']}}))
    stub = isolated / 'bin/job'
    stub.write_text('#!/bin/sh\ncase "$1" in\nsync) exit 0;;\nagents) printf %s "$3" > stored.json;;\n*) exit 9;;\nesac\n')
    stub.chmod(0o755)
    result = call(isolated, 'fixture')
    assert result.returncode == 0, result.stderr
    assert json.loads((isolated.parent / 'stored.json').read_text()) == {
        'reviewers': {'code': code, 'security': 'opus'}}
