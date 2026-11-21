Feature: Map artwork to archived prompts

  Rule: Only a digest of the prompt text proves its identity

    Scenario: An exact prompt is no longer on the current branch
      Given the configured filename grammar captures a version digest and optionally a lineage
      When any Git blob, even an unreachable one, has that content digest under the configured algorithm
      Then the immutable prompt text is Exact regardless of its Git object ID or basename at HEAD
      And the archive is not modified

    Scenario: Only the lineage matches
      Given no blob has the version digest but history contains candidates for the lineage
      When Pick or Calendar displays the nearest candidates
      Then each line names its lineage path and appears under a not archived / not authoritative warning
      And no candidate text is presented as Exact

    Scenario: A lineage path contains tabs, quotes, backslashes or newlines
      When reading NUL-delimited history from the configured repository
      Then the lineage path round-trips verbatim, independent of inherited Git directory, work tree or index settings

  Rule: Search limits count available artwork, not archive hits

    Scenario: Return exact-text hits before explicit lineage matches
      When the user searches for prompt text
      Then only Exact prompt text matches the text query; nearest text does not
      And explicit lineage matches can follow the Exact results as Maybe hits

    Scenario: More archive hits than available files
      Given some matches are referenced, unreadable or outside the filename grammar
      When searching with a result limit
      Then those files do not consume the limit
      And truncation is reported only after finding one additional available result, at which point hashing stops

  Rule: Refresh may change availability, not provenance

    Scenario: An unreachable blob is added without changing HEAD
      When the user refreshes the UI or runs prompt-audit again
      Then the index is rebuilt even though HEAD is unchanged
      And between refreshes automatic rebuilds follow HEAD changes, checked at most once every two seconds

    Scenario: The archive becomes unavailable during a rebuild
      When the next lookup needs the index
      Then it reports Unknown with a prompt archive unavailable diagnostic, not an exact match or a partial index
      And the previous index stays whole and the next lookup retries

    Scenario: Audit filenames after the producer changes their grammar
      When fewer files match the configured grammar
      Then the audit reports the lower parsed count rather than inferring a new delimiter or digest algorithm
