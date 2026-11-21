Feature: Data-directory configuration

  Rule: Configuration follows the data directory, not the publication file

    Scenario: Open a publication file outside the data directory
      Given --data-dir selects a directory with publicator.toml and --json names an external file
      When the UI or CLI opens the store
      Then artwork roots, tags, prompt archive and DeviantArt allow-lists come from the selected directory
      And the schema, worker and publishing skill come from the application package

    Scenario: Start without an explicit directory or config
      Given no --data-dir and no publicator.toml in the working directory
      When Publicator starts
      Then it uses the working directory and the configuration defaults
      And publishing opens a visible browser unless the CLI forces --headless

    Scenario: Enable headless publishing in the config
      Given [browser].headless is true
      When the user publishes from the UI or CLI without an override
      Then the browser runs headless

  Rule: Invalid input fails before mutation or provider work

    Scenario Outline: Reject bad settings before using the store
      Given publicator.toml contains <error>
      When the configuration is loaded
      Then the error is reported instead of silently using defaults

      Examples:
        | error                                        |
        | invalid TOML                                 |
        | a non-boolean browser.headless               |
        | an invalid schedule timezone or weekday      |
        | an invalid slot hour or capacity             |
        | empty or duplicate schedule profiles         |
        | a profile with an unconfigured tier/gallery  |
        | a declared prompt grammar without version    |

    Scenario: Optional prompt lookup is not configured
      Given no prompt archive is configured
      When the user opens Pick
      Then prompt lookup is disabled and no filename grammar is guessed

    Scenario: Narrow the search at runtime
      Given publicable roots A and B are configured
      When the user selects only B for this search
      Then Pick searches B without changing publicator.toml or accepting an arbitrary root
