Feature: Publish approved artwork to DeviantArt without sending twice

  Rule: Rust owns approval, the store and the durable record of each send

    Scenario: Dispatch a selected batch
      Given the complete document, target options, tags, price, schedule and artwork pass validation
      When a fresh read confirms approval and an unpublished DeviantArt target
      Then Rust writes a durable intent before dispatching one staged, hash-verified artwork
      And persists the result before the next send; delivery recorded since approval stops the batch
      And the worker never reads the mutable original, publication database or config

    Scenario: Content or metadata no longer matches approval
      When the title or description is unusable or no visible file matches the recorded SHA-512
      Then the artwork is not sent, even if another file has the same name

  Rule: Worker trust boundary

    Scenario: Open a browser session
      Given the worker is bound at startup to the store's data root, not a root supplied in protocol messages
      When opening the session
      Then the only profile paths allowed are its .deviantart-login and .deviantart-session children
      And publishing uses a disposable session copy, never the original human login profile
      And the browser steps follow the order checked against the static publishing skill

    Scenario: Reject a bad job before touching a browser session
      When decoding a version 1 UTF-8 NDJSON frame
      Then frames over 1 MiB, unexpected fields and invalid IDs are refused
      And the worker rejects non-finite or negative prices, WebP suffixes, nonzero minutes and wall times inconsistent with the epoch and configured timezone
      And a rejected submit frame produces a fatal reply with no browser submission

    Scenario: Observe the final Schedule click
      When the worker clicks Schedule
      Then scheduled requires a timely success status in the JSON reply to POST /_napi/shared_api/deviation/update, not the studio URL or the click alone
      And a valid returned HTTP(S) deviation URL is saved to the receipt and apparition; an invalid URL is dropped
      And any error after the click is unknown; a failure before the click is failed

  Rule: Reconcile an uncertain remote outcome before retry or manual upload

    Scenario: The worker disappears or sends an unusable result after dispatch
      Given the submit might have reached DeviantArt
      When the worker exits, truncates its reply, reports unknown or returns a result for another attempt
      Then the receipt stays unresolved, the batch stops and no retry resends that artwork
      And the CLI lists UNRESOLVED with the attempt ID, UUID, reason and both reconciliation commands

    Scenario Outline: A human reconciles after inspecting DeviantArt
      Given unknown is nonzero or a later run reports reconciliation required
      When the human verifies <evidence> and runs <command> for the blocked attempt
      Then <result>
      And the decision is recorded under the publisher lock without deleting the receipt

      Examples:
        | evidence                      | command                                                          | result                                   |
        | the submission is present     | da-publish --reconcile-attempt ID --outcome scheduled [--url URL] | the delivery is recorded, not resent     |
        | the submission is absent      | da-publish --reconcile-attempt ID --outcome not-submitted         | retry or manual upload is permitted      |

    Scenario: Reconcile the same attempt again
      When repeating a decision on an applied or released attempt
      Then the same decision is idempotent and a different one is refused without changes

    Scenario: A safe failure leaves a possible draft
      Given unknown is zero, there is no UNRESOLVED line and the run failed before submission
      When preparing a manual retry
      Then the operator checks for a leftover draft before following the static skill's manual steps

    Scenario: Inspect the journal after interruption
      When recovery or status finds an attempt directory without receipt.json
      Then it leaves the directory alone and skips it; reconciliation reports that nothing was dispatched
      But an empty, malformed or symlinked receipt stops recovery for investigation
      And a valid terminal receipt can be replayed across restart, including an already-applied success

  Rule: Worker cleanup finishes before the publisher lock is released

    Scenario: The worker ignores graceful shutdown
      When the last terminal result has been received
      Then the parent sends close, drops stdin and waits up to five seconds before terminating the group
      And after another cleanup deadline it kills and reaps survivors; a held-open stderr cannot block shutdown indefinitely

    Scenario: Publishing is interrupted
      When the step check times out at 60 seconds, or the running publish is aborted or signalled
      Then the owned process group is cleaned up before releasing .publicator/publish.lock
      And an already dispatched attempt without a terminal receipt remains blocked for reconciliation

    Scenario: Start the developer DOM prober
      When pw-daemon starts independently of login and publishing
      Then its command FIFO, log and screenshot are confined to a fresh private 0700 temporary directory
      And it refuses existing endpoints before opening a browser
