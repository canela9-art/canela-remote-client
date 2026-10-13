// CanelaRemote (parche 25): al terminar un soporte, la App Técnico pregunta qué se hizo.
//
// canela_activity.rs avisa a la ventana principal con el evento global `canela_session_done`
// ({peer, peer_name, seconds}) cuando se cierra una sesión de más de un minuto. Aquí se muestra el
// cuadro con los soportes predeterminados del panel (GET /api/canela/support-types) para marcar con
// un clic y un detalle opcional, y se guarda en el soporte (POST /api/canela/session-done).
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:get/get.dart';
import 'package:window_manager/window_manager.dart';

import '../utils/http_service.dart' as http;

class _SupportType {
  final String id;
  final String name;
  const _SupportType(this.id, this.name);
}

/// Equipos con el cuadro abierto (dos ventanas al mismo equipo no preguntan dos veces).
final Set<String> _asking = {};

/// Se llama una vez desde la ventana principal.
void canelaRegisterSessionDone() {
  if (!isDesktop || !canelaIsTecnico()) return;
  platformFFI.registerEventHandler(
      'canela_session_done', 'canela_session_done', (evt) async {
    final peer = '${evt['peer'] ?? ''}'.trim();
    if (peer.isEmpty || _asking.contains(peer)) return;
    final secs = int.tryParse('${evt['seconds'] ?? 0}') ?? 0;
    final name = '${evt['peer_name'] ?? ''}'.trim();
    _asking.add(peer);
    try {
      await _ask(peer, name, secs);
    } finally {
      _asking.remove(peer);
    }
  });
}

Future<String> _api(String path) async =>
    '${await bind.mainGetApiServer()}/api/canela/$path';

Future<List<_SupportType>> _loadTypes() async {
  try {
    final resp =
        await http.get(Uri.parse(await _api('support-types')), headers: getHttpHeaders());
    if (resp.statusCode != 200) return [];
    final items = (jsonDecode(resp.body)['items'] as List?) ?? [];
    return items
        .map((e) => _SupportType('${e['id']}', '${e['name']}'))
        .toList();
  } catch (e) {
    debugPrint('canela session_done: $e');
    return [];
  }
}

/// null = guardado; si no, el mensaje de error para el técnico.
Future<String?> _send(String peer, String? typeId, String detail) async {
  try {
    final headers = getHttpHeaders();
    headers['Content-Type'] = 'application/json';
    final resp = await http.post(Uri.parse(await _api('session-done')),
        headers: headers,
        body: jsonEncode(
            {'peer': peer, 'support_type_id': typeId, 'detail': detail}));
    if (resp.statusCode == 200) return null;
    final msg = (jsonDecode(resp.body) as Map?)?['message'];
    return msg is String && msg.isNotEmpty
        ? msg
        : 'No se pudo guardar (${resp.statusCode}).';
  } catch (e) {
    return 'No se pudo guardar. Revisa la conexión e intenta de nuevo.';
  }
}

String _minutes(int secs) {
  final m = (secs / 60).round();
  return m < 60 ? '$m min' : '${m ~/ 60} h ${m % 60} min';
}

Future<void> _ask(String peer, String name, int secs) async {
  final types = await _loadTypes();
  try {
    await windowManager.show();
    await windowManager.focus();
  } catch (_) {}
  String? selected;
  String? error;
  var busy = false;
  final text = TextEditingController();
  final who = name.isEmpty ? peer : '$name ($peer)';

  await gFFI.dialogManager.show<void>((setState, close, context) {
    Future<void> save() async {
      final detail = text.text.trim();
      if (selected == null && detail.isEmpty) {
        setState(() => error = 'Marca un soporte o escribe qué se hizo.');
        return;
      }
      setState(() {
        busy = true;
        error = null;
      });
      final err = await _send(peer, selected, detail);
      if (err == null) {
        close();
        showToast('Soporte documentado');
      } else {
        setState(() {
          busy = false;
          error = err;
        });
      }
    }

    return CustomAlertDialog(
      title: const Text('¿Qué se hizo en este soporte?'),
      contentBoxConstraints: const BoxConstraints(maxWidth: 560),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('$who · ${_minutes(secs)}',
              style: TextStyle(color: Theme.of(context).hintColor)),
          if (types.isNotEmpty)
            Wrap(
              spacing: 6,
              runSpacing: 6,
              children: types
                  .map((t) => ChoiceChip(
                        label: Text(t.name),
                        selected: selected == t.id,
                        onSelected: busy
                            ? null
                            : (v) => setState(() => selected = v ? t.id : null),
                      ))
                  .toList(),
            ).marginOnly(top: 14),
          TextField(
            controller: text,
            minLines: 2,
            maxLines: 4,
            maxLength: 2000,
            enabled: !busy,
            decoration: const InputDecoration(
              hintText: 'Detalle (opcional): qué se encontró y qué se hizo',
            ),
          ).marginOnly(top: 14),
          if (error != null)
            Text(error!, style: const TextStyle(color: Colors.red))
                .marginOnly(top: 6),
        ],
      ),
      actions: [
        dialogButton('Omitir', onPressed: busy ? null : close, isOutline: true),
        dialogButton('Guardar', onPressed: busy ? null : save),
      ],
      onCancel: busy ? null : close,
    );
  }, tag: 'canela-session-done-$peer');
}
