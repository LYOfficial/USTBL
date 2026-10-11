/**
 * Helpers for the notifications pushed to the tray popup window (bottom-right).
 *
 * The backend command `show_message_notification` only carries a plain message
 * string, so update notifications are tagged with a marker. That lets the tray
 * popup render a dedicated title/body for a launcher update instead of the
 * generic message wording, without touching the Rust side.
 */

const UPDATE_NOTIFICATION_PREFIX = "ustbl:update:";

export const buildUpdateNotificationMessage = (version: string): string =>
  `${UPDATE_NOTIFICATION_PREFIX}${version}`;

/**
 * Extract the version from an update notification message.
 * @returns The version, or null when the message is a regular message.
 */
export const parseUpdateNotificationVersion = (
  message: string
): string | null =>
  message.startsWith(UPDATE_NOTIFICATION_PREFIX)
    ? message.slice(UPDATE_NOTIFICATION_PREFIX.length)
    : null;
