Feature: Discover artwork and prepare previews and uploads

  Rule: Pick, prompt search and candidate deletion share the same reference policy

    Scenario: Scan overlapping configured roots
      Given two configured roots inside the data directory contain byte-identical files
      When scanning for jpg, jpeg, png, webp, gif, bmp, tiff or mp4 in any letter case
      Then there is at most one candidate per content hash and .tif is not offered
      And a root symlink may resolve inside the data directory but symlink entries beneath it are never followed

    Scenario: Skip a protected candidate after its contents change
      Given any publication references a basename or SHA-512 in files or previewFilesIfNotFree
      When discovering, searching or deleting candidates
      Then that basename and matching content remain protected, even if the named file has changed
      And recorded hashes compare without regard to letter case

    Scenario: An entry cannot be read
      When discovery cannot read a file
      Then it logs the error and skips that file
      But an unreadable directory fails the search instead of hiding its contents

    Scenario: The authorized candidate changes before deletion
      When Linux candidate deletion rechecks the store references, parent identity and file identity
      Then only the authorized file under the configured root may be unlinked
      And a mismatch fails without replacing a newer entry; a failed restore reports the private recovery path
      And discovery ignores private deletion remnants after a crash; other operating systems refuse deletion

  Rule: A persistent preview belongs to one recorded content identity

    Scenario: Resolve a row among namesakes
      Given a row records sha512sum or, if absent, fileSha512sum
      When a workflow row or calendar event requests a preview
      Then same-named files are distinguished by hash; missing identity is an error, not a guess
      And an edited lone original may reuse its recorded cached preview but cannot be published as the original

    Scenario: Concurrent cache misses for the same hash
      When a thumbnail is prepared in .thumbs/ from a private copy verified against the cache key
      Then simultaneous creators publish only complete decodable entries by atomic rename
      And a changed source or conversion failure caches nothing, so a later request can retry

    Scenario Outline: A cache entry already exists
      When the cache name contains <entry>
      Then the preview request must <outcome>

      Examples:
        | entry                    | outcome                                             |
        | a decodable regular file | reuse the preview across runs                       |
        | corrupt bytes            | rebuild and atomically replace the entry            |
        | a symlink                | replace the link without writing through it         |
        | a directory              | fail without removing the directory                 |

  Rule: A preview is not an upload substitute

    Scenario: Decode animated or multipage artwork
      When an admitted GIF, WebP or TIFF contains multiple frames
      Then detection, preview and AI input use the first frame
      And a GIF upload retains the animation; an animated WebP is refused rather than uploaded as a still

    Scenario: Decode a damaged image
      When the decoder warns, even for a truncated JPEG it can partially display
      Then preview, AI input and upload are refused

    Scenario: Prepare a still WebP for DeviantArt
      Given WebP content or a .webp suffix
      When preparing the staged upload under .publicator/
      Then it is converted to a validated PNG with alpha preserved
      And no upload containing WebP bytes is handed to the worker
