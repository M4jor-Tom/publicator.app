Feature: Scheduling publications

  Rule: Weekly slots follow the timezone in the data directory's [schedule] settings

    Scenario: A weekly slot crosses a daylight-saving change
      Given no timezone override and a Tuesday 20:00 profile
      When the clock changes in Europe/Paris
      Then the slot is still at 20:00 Paris time, regardless of the computer's timezone

  Rule: Repacking assigns automatic PICKED dates in stored order, within capacity

    Scenario: A partially delivered publication counts once, even while PICKED
      Given the next two weekly slots each have capacity for two publications
      And a PICKED publication in the first slot has two published_or_scheduled deliveries
      And undelivered automatic PICKED publications A and B follow it in that order
      When dates are repacked
      Then the delivered publication keeps its date
      And A gets the remaining place in the first slot
      And B gets the second slot

    Scenario: Manual pins may exceed capacity
      Given two PICKED publications pinned to the same one-place slot
      When dates are repacked
      Then both keep their dates and no automatic publication gets that slot

    Scenario: All 52 weekly slots are full
      Given approved publications fill every slot in a profile's 52-week horizon
      When dates are repacked for another automatic PICKED publication on that profile
      Then it has no suggested date and the approved dates do not move

  Rule: A user can pin, release or approve a date without selecting a platform

    Scenario: Approve a date outside the preset slots
      Given an undelivered PICKED publication with no platform selected
      When the user saves a custom time outside the preset slots and approves it
      Then the publication is SCHEDULED at that exact time with no platform selected
      And later repacking does not move it

    Scenario Outline: Return a pinned publication to automatic allocation
      Given an undelivered PICKED publication with a manually pinned date
      And the profile has an available slot
      When the user chooses <choice>
      Then the pin is cleared and the publication gets the first available slot for that profile

      Examples:
        | choice                      |
        | a configured preset         |
        | the default date suggestion |

    Scenario: The saved preset no longer exists
      Given a PICKED publication has a saved date for a removed preset
      When dates are repacked
      Then its saved date and profile name are unchanged
      And the Schedule tab shows the date as custom rather than selecting a different preset

    Scenario: Return to scheduling without a date override
      Given an undelivered SCHEDULED publication has no date override
      When the user returns it to scheduling
      Then it is PICKED and still has no date override
