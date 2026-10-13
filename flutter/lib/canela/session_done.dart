// CanelaRemote (parche 25): al terminar un soporte, la App Técnico pregunta qué se hizo.
//
// canela_activity.rs avisa a la ventana principal con el evento global `canela_session_done`
// ({peer, peer_name, seconds}) cuando se cierra una sesión de más de un minuto. Aquí se muestra el
// cuadro con los soportes predeterminados del panel (GET /api/canela/support-types) para marcar con
// un clic y un detalle opcional, y se guarda en el soporte (POST /api/canela/session-done).
//
// Casos (migración 014): las conexiones del mismo técnico al mismo equipo el mismo día se juntan solas
// en un soporte. Antes de preguntar se consulta con cuál quedó enlazada (GET /api/canela/session-case)
// y, si ya había otras conexiones, el técnico confirma "es continuación" o dice "es un caso nuevo".
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

/// Caso con el que quedó enlazada la conexión (null si no se pudo consultar).
class _Case {
  final int connections;
  final String? typeId;
  final String typeName;
  final String resolution;
  final DateTime? firstAt;
  const _Case(this.connections, this.typeId, this.typeName, this.resolution, this.firstAt);
}

Future<_Case?> _loadCase(String peer) async {
  try {
    final uri = Uri.parse(await _api('session-case?peer=${Uri.encodeQueryComponent(peer)}'));
    final resp = await http.get(uri, headers: getHttpHeaders());
    if (resp.statusCode != 200) return null;
    final c = jsonDecode(resp.body)['case'];
    if (c is! Map) return null;
    return _Case(
      int.tryParse('${c['connections'] ?? 1}') ?? 1,
      c['support_type_id'] as String?,
      '${c['support_type_name'] ?? ''}',
      '${c['resolution'] ?? ''}',
      DateTime.tryParse('${c['first_at'] ?? ''}')?.toLocal(),
    );
  } catch (e) {
    debugPrint('canela session_done: $e');
    return null;
  }
}

String _hour(DateTime? d) {
  if (d == null) return '';
  final h = d.hour % 12 == 0 ? 12 : d.hour % 12;
  final m = d.minute.toString().padLeft(2, '0');
  return ' que empezó a las $h:$m ${d.hour < 12 ? 'a. m.' : 'p. m.'}';
}

/// null = guardado; si no, el mensaje de error para el técnico.
Future<String?> _send(String peer, String? typeId, String detail, bool newCase) async {
  try {
    final headers = getHttpHeaders();
    headers['Content-Type'] = 'application/json';
    final resp = await http.post(Uri.parse(await _api('session-done')),
        headers: headers,
        body: jsonEncode(
            {'peer': peer, 'case': newCase ? 'new' : 'continue', 'support_type_id': typeId, 'detail': detail}));
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
  final linked = await _loadCase(peer);
  // ya había otras conexiones hoy a este equipo: se le enseña con cuál se enlaza
  final linkedConns = linked?.connections ?? 1;
  final linkedTypeId = linked?.typeId;
  final linkedTypeName = linked?.typeName ?? '';
  final linkedRes = linked?.resolution ?? '';
  final isContinuation = linkedConns > 1;
  try {
    await windowManager.show();
    await windowManager.focus();
  } catch (_) {}
  var newCase = false;
  String? selected = isContinuation ? linkedTypeId : null;
  String? error;
  var busy = false;
  final text = TextEditingController(
      text: isContinuation && linkedRes != linkedTypeName ? linkedRes : '');
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
      final err = await _send(peer, selected, detail, newCase);
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
          if (isContinuation)
            Container(
              padding: const EdgeInsets.all(10),
              decoration: BoxDecoration(
                border: Border.all(color: Theme.of(context).dividerColor),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    'Hoy ya te conectaste a este equipo: se enlaza con el soporte'
                    '${_hour(linked?.firstAt)} (${linkedConns - 1} conexión'
                    '${linkedConns - 1 == 1 ? '' : 'es'} antes)'
                    '${linkedTypeName.isNotEmpty ? ' · $linkedTypeName' : ''}.',
                  ),
                  RadioListTile<bool>(
                    dense: true,
                    contentPadding: EdgeInsets.zero,
                    value: false,
                    groupValue: newCase,
                    title: const Text('Es el mismo problema (continuación)'),
                    onChanged: busy
                        ? null
                        : (_) => setState(() {
                              newCase = false;
                              selected = linkedTypeId;
                            }),
                  ),
                  RadioListTile<bool>(
                    dense: true,
                    contentPadding: EdgeInsets.zero,
                    value: true,
                    groupValue: newCase,
                    title: const Text('Es otro problema (soporte nuevo)'),
                    onChanged: busy
                        ? null
                        : (_) => setState(() {
                              newCase = true;
                              selected = null;
                              text.clear();
                            }),
                  ),
                ],
              ),
            ).marginOnly(top: 12),
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
