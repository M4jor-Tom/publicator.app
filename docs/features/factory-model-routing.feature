Feature: Factory model routing
  Scenario: Review risk selects a tier before the harness selects a model
    Given the same code reviewer is requested on a low-risk and a high-risk job
    When the factory records the reviewer routes
    Then the jobs record haiku and sonnet respectively, not GPT model IDs
    And Pi resolves the first to Luna and the second to Terra

  Scenario: An unmapped tier cannot launch on Pi
    Given a requested tier has no provider-qualified mapping
    When the Pi orchestrator resolves the tier
    Then lookup fails without returning a model or modifying a job

  Scenario: Each harness uses its native builder and the same factory gate
    Given the builder tier is opus and the job already has a factory worktree
    When a builder is started for the job
    Then Claude Code uses its builder profile and Pi explicitly selects Sol
    And a Pi retry in the same session resumes the latest builder run for that job
    And neither harness starts reviewers before the factory gate passes
