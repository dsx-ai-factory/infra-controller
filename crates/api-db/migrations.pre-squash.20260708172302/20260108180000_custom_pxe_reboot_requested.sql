-- Add custom_pxe_reboot_requested column to instances table.
-- This flag is set by the API when a tenant requests a reboot with custom iPXE.
-- It is checked by the Ready handler to initiate the HostPlatformConfiguration flow.
-- It is cleared by the WaitingForRebootToReady handler after setting use_custom_pxe_on_boot.
-- This separates the "trigger reboot" concern from the "serve iPXE script" concern,
-- eliminating race conditions in the state machine.

ALTER TABLE instances ADD COLUMN custom_pxe_reboot_requested BOOLEAN NOT NULL DEFAULT FALSE;

