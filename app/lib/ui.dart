import 'package:flutter/material.dart';

/// Shared UI building blocks used by both the Watch and Beam tabs, so the two
/// stay visually identical (a device is a device; a status is a status) with a
/// single implementation.

/// A titled section header with an optional trailing widget (a count, spinner…).
class SectionHeader extends StatelessWidget {
  const SectionHeader(this.title, {this.trailing, super.key});

  final String title;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Row(
        children: [
          Text(title, style: theme.textTheme.titleMedium),
          if (trailing != null) ...[const Spacer(), trailing!],
        ],
      ),
    );
  }
}

/// One device in a list — a discovered host to watch, or a client connected to
/// this beam. Highlighted when [active], optionally tappable, with trailing
/// actions (a status chip, connect/stop/forget buttons).
class DeviceTile extends StatelessWidget {
  const DeviceTile({
    required this.icon,
    required this.title,
    this.subtitle,
    this.active = false,
    this.trailing,
    this.onTap,
    super.key,
  });

  final IconData icon;
  final String title;
  final String? subtitle;
  final bool active;
  final Widget? trailing;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Material(
        color: active
            ? theme.colorScheme.primaryContainer.withValues(alpha: 0.45)
            : theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(12),
        clipBehavior: Clip.antiAlias,
        child: ListTile(
          enabled: onTap != null,
          leading: Icon(
            icon,
            color: active
                ? theme.colorScheme.primary
                : theme.colorScheme.onSurfaceVariant,
          ),
          title: Text(title),
          subtitle: subtitle != null ? Text(subtitle!) : null,
          trailing: trailing,
          onTap: onTap,
        ),
      ),
    );
  }
}

/// A small rounded status pill (e.g. "Online" / "Offline" / "Connected").
class StatusChip extends StatelessWidget {
  const StatusChip({required this.label, this.highlight = false, super.key});

  final String label;
  final bool highlight;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final bg = highlight
        ? theme.colorScheme.primaryContainer
        : theme.colorScheme.surfaceContainerHighest;
    final fg = highlight
        ? theme.colorScheme.onPrimaryContainer
        : theme.colorScheme.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(12)),
      child: Text(label, style: theme.textTheme.labelSmall?.copyWith(color: fg)),
    );
  }
}

/// An inline status / notice message with optional copy + dismiss actions.
class StatusNotice extends StatelessWidget {
  const StatusNotice({
    required this.message,
    this.isError = false,
    this.onCopy,
    this.onDismiss,
    super.key,
  });

  final String message;
  final bool isError;
  final VoidCallback? onCopy;
  final VoidCallback? onDismiss;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final fg = isError
        ? theme.colorScheme.onErrorContainer
        : theme.colorScheme.onSurfaceVariant;

    return Material(
      color: isError
          ? theme.colorScheme.errorContainer
          : theme.colorScheme.surfaceContainerHighest,
      borderRadius: BorderRadius.circular(8),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 12, 4, 12),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (isError) ...[
              Icon(Icons.error_outline, color: fg, size: 20),
              const SizedBox(width: 8),
            ],
            Expanded(
              child: SelectableText(
                message,
                style: theme.textTheme.bodyMedium?.copyWith(color: fg),
              ),
            ),
            if (onCopy != null)
              IconButton(
                onPressed: onCopy,
                tooltip: 'Copy',
                icon: Icon(Icons.copy, size: 20, color: fg),
                visualDensity: VisualDensity.compact,
              ),
            if (onDismiss != null)
              IconButton(
                onPressed: onDismiss,
                tooltip: 'Dismiss',
                icon: Icon(Icons.close, size: 20, color: fg),
                visualDensity: VisualDensity.compact,
              ),
          ],
        ),
      ),
    );
  }
}
