Feature: Generate and approve artwork metadata

  Rule: Admission validates and reserves the work before contacting a provider

    Scenario: Invalid source or stale batch selection
      Given the publication document or config is invalid, or the expected batch IDs differ in membership or order
      When the user requests generation
      Then no llmfit, Ollama or OpenRouter request is made and the source bytes are unchanged

    Scenario: Generate from the recorded artwork
      Given two visible files have the same basename
      When generating for files[0]
      Then only bytes matching its recorded SHA-512 may supply the AI input, including an image/webp preview
      And changed artwork with no matching copy requires reconciliation without a provider call or metadata write

  Rule: The application owns approval, not the provider

    Scenario: Accept a complete pair
      Given a SCHEDULED publication whose admitted fields have not changed
      When the chosen provider returns both title and description
      Then a validated, nonblank pair is saved and the publication becomes DESCRIBED

    Scenario: Generation fails after admission
      Given a SCHEDULED publication whose admitted fields have not changed
      When its single provider generation fails
      Then nothing is written: the title and description stay unchanged
      And the publication remains SCHEDULED and retryable, not approved for publishing
      And its card shows "Failed: <error>"

    Scenario: Another writer edits a field during single generation
      Given the application admitted the current title and description, including any saved draft
      When a writer changes the title before generation completes
      Then the generated title does not replace the newer title
      And only fields still equal to their admitted values may be replaced, without approving the pair

    Scenario: Run a marked batch
      Given the batch admits every SCHEDULED publication whose title or description is blank after trimming
      When the admitted publications are processed in order
      Then one record runs at a time outside the store lock
      And only fields blank when the record started and unchanged since are merged; it becomes DESCRIBED when a merged field leaves a valid pair
      And each start and finish is reported and each success persisted before the next record
      And a finished record becomes editable immediately; aborting releases all remaining reservations

  Rule: Provider choice, consent and deadlines are explicit

    Scenario: OpenRouter has no credential
      Given neither [openrouter].key in the data directory's private publicator.local.toml nor a nonempty OPENROUTER_KEY
      When OpenRouter is selected
      Then generation reports "OPENROUTER_KEY is not set" before fetching even the model catalogue

    Scenario: Read a data-directory credential
      Given the private publicator.local.toml contains a nonempty [openrouter].key
      When resolving OpenRouter credentials
      Then the local key takes precedence over OPENROUTER_KEY
      But a malformed secrets file reports an error without echoing its source line

    Scenario: OpenRouter rate-limits a request
      When any OpenRouter response returns HTTP 429 headers
      Then new OpenRouter completions, schema and fallback attempts, and uncached catalogue requests share one pause on that client
      And in-flight requests may finish and save normally; Ollama is not paused
      And a usable Retry-After delay or HTTP-date sets the pause without a backoff cap
      But a missing or unusable value backs off for 1, 2, 4, then at most 8 seconds until work dispatched after the latest pause succeeds
      And a concurrent 429 can extend, but never shorten, the pause
      And the exact throttled request retries without changing model or schema mode

    Scenario: The active record's deadline expires during an OpenRouter pause
      Given each sequential record starts its own deadline when processing begins
      When preparation, provider work and cooldown use up that deadline
      Then no retry is sent and the record reports one OpenRouter deadline failure, even if throttled during fallback or catalogue lookup
      And the failed record remains unchanged and eligible while the next record starts with its own budget
      And cancelling a waiting run releases its reservations without erasing the shared pause or leaving a retry behind

    Scenario: Ollama is missing a fitting model
      Given local generation uses an already-running Ollama service and llmfit assessed the model
      When the model is not installed
      Then fetching it requires explicit consent; Publicator does not start Ollama or fall back to OpenRouter
      And only the CLI accepts PUBLICATOR_OLLAMA_URL as an endpoint override

    Scenario Outline: Distinguish an unavailable local service from an invalid reply
      When /api/tags <result>
      Then local generation reports <diagnostic>

      Examples:
        | result                                      | diagnostic       |
        | refuses, times out or returns an HTTP error | unavailability   |
        | returns a body that is not tags JSON        | a protocol error |

    Scenario: Preparation stalls
      When the deadline expires during AI image preparation
      Then the conversion process group is killed and reaped
      And preparation and provider calls share one deadline with preflight for single generation
      But a batch has one preflight deadline and a separate preparation/provider deadline per record
