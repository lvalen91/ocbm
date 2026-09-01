package com.carlink.projection

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.SharedPreferences
import android.os.Bundle
import android.os.IBinder
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/**
 * The Projection device screen — connect, forget, and the auto-connect policy.
 *
 * ## Why this is a separate screen from Settings ▸ Bluetooth
 *
 * It has to be. §2: `carplay-wireless` owns `hci0` directly and Android's Bluetooth stack is
 * disabled so it can, which means the stock Bluetooth pane has no stack behind it and cannot pair
 * or list anything. Every device shown here comes from [WirelessControlClient].
 *
 * That constraint lands us exactly where the OEM already is: GM does **not** reuse its generic
 * Bluetooth phone list for projection either — `ProjectionPhonesActivity` is a separate screen next
 * to `PhonesActivity`. Arriving at the same split from a different direction is reassuring.
 *
 * This activity is exported and carries the Settings-injection metadata so it can appear inside
 * AAOS Settings rather than only as its own launcher entry (§7.3).
 */
class DeviceManagerActivity : ComponentActivity() {
    private var service: ProjectionService? = null
    private var bound = false
    private val control = WirelessControlClient(SessionStatus())

    private val conn =
        object : ServiceConnection {
            override fun onServiceConnected(
                name: ComponentName?,
                binder: IBinder?,
            ) {
                service = (binder as? ProjectionService.LocalBinder)?.service
            }

            override fun onServiceDisconnected(name: ComponentName?) {
                service = null
            }
        }

    private lateinit var prefs: SharedPreferences

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        ProjectionService.start(this)
        setContent {
            MaterialTheme {
                DeviceScreen(
                    control = control,
                    initialAutoConnect = prefs.getBoolean(KEY_AUTO_CONNECT, true),
                    onAutoConnectChanged = { on ->
                        prefs.edit().putBoolean(KEY_AUTO_CONNECT, on).apply()
                        // The switch is the BOX's policy, not a local preference: `reconnect` reads
                        // it every round. Without this push the setting was WRITE-ONLY — it updated
                        // SharedPreferences and the box was never told, so "tap to connect" never
                        // took effect and the toggle did nothing at all.
                        //
                        // `order` is deliberately omitted rather than sent empty: the box treats an
                        // absent field as "leave it alone", and sending an empty list would wipe a
                        // stored first-to-connect order the UI does not yet manage.
                        Thread { control.setAutoConnect(on) }.start()
                    },
                    onOpenProjection = { ProjectionActivity.bringToFront(this) },
                )
            }
        }
    }

    override fun onStart() {
        super.onStart()
        bound = bindService(Intent(this, ProjectionService::class.java), conn, Context.BIND_AUTO_CREATE)
    }

    override fun onStop() {
        super.onStop()
        if (bound) {
            runCatching { unbindService(conn) }
            bound = false
        }
    }

    companion object {
        const val PREFS = "projection"
        const val KEY_AUTO_CONNECT = "autoConnect"
    }
}

@Composable
private fun DeviceScreen(
    control: WirelessControlClient,
    initialAutoConnect: Boolean,
    onAutoConnectChanged: (Boolean) -> Unit,
    onOpenProjection: () -> Unit,
) {
    var devices by remember { mutableStateOf<List<WirelessControlClient.Device>>(emptyList()) }
    var radioUp by remember { mutableStateOf(false) }
    var sessionActive by remember { mutableStateOf(false) }
    var activeAddress by remember { mutableStateOf<String?>(null) }
    var autoConnect by remember { mutableStateOf(initialAutoConnect) }
    var busy by remember { mutableStateOf(false) }

    // Poll the radio owner. The control socket is a blocking loopback round trip, so it must not run
    // on the composition thread — every call is dispatched to IO.
    LaunchedEffect(Unit) {
        while (true) {
            val list = withContext(Dispatchers.IO) { control.list() }
            val st = withContext(Dispatchers.IO) { control.status() }
            devices = list
            radioUp = control.available
            sessionActive = st?.active == true
            activeAddress = st?.address
            delay(REFRESH_MS)
        }
    }

    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text("CarPlay", style = MaterialTheme.typography.headlineMedium)

        if (!radioUp) {
            // Naming the cause matters: "no devices" and "the radio owner is not running" look
            // identical in the UI and have completely different fixes.
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp)) {
                    Text("Projection service unavailable", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "carplay-wireless is not responding on port ${SeamContract.PORT_CONTROL}. " +
                            "No phone can be paired or connected until it is running.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        }

        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Connect automatically", style = MaterialTheme.typography.titleMedium)
                Text(
                    "Try known phones in order when the car starts. Off means tap to connect.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Switch(
                checked = autoConnect,
                onCheckedChange = { on ->
                    autoConnect = on
                    onAutoConnectChanged(on)
                },
            )
        }

        Text("Phones", style = MaterialTheme.typography.titleLarge)

        if (devices.isEmpty() && radioUp) {
            Text(
                "No phones yet. On the phone, open Bluetooth settings and select this car.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        if (sessionActive && activeAddress == null) {
            // A session is live but the box cannot name the phone (the accept path is handed an
            // already-open socket). Showing Connect/Forget for every row here is how the live
            // phone's link key gets deleted — so disable both and say why rather than guess which
            // row is the live one.
            Text(
                "A phone is connected. Which one cannot be identified yet, so Connect and Forget " +
                    "are disabled.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(devices, key = { it.address }) { d ->
                DeviceRow(
                    device = d,
                    enabled = !busy && !(sessionActive && activeAddress == null),
                    onConnect = {
                        busy = true
                        Thread {
                            control.connect(d.address)
                            busy = false
                        }.start()
                    },
                    onDisconnect = {
                        busy = true
                        Thread {
                            control.disconnect()
                            busy = false
                        }.start()
                    },
                    onForget = {
                        busy = true
                        Thread {
                            control.forget(d.address)
                            busy = false
                        }.start()
                    },
                    onOpen = onOpenProjection,
                )
            }
        }
    }
}

@Composable
private fun DeviceRow(
    device: WirelessControlClient.Device,
    enabled: Boolean,
    onConnect: () -> Unit,
    onDisconnect: () -> Unit,
    onForget: () -> Unit,
    onOpen: () -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(device.name, style = MaterialTheme.typography.titleMedium)
                Text(
                    when {
                        device.connected -> "Connected"
                        device.bonded -> "Paired"
                        else -> device.address
                    },
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            if (device.connected) {
                // "Open" rather than "Connect": the session already exists, so this is a foreground
                // change, not a connection. Offering "Connect" here is how a UI ends up tearing down
                // a working session to rebuild it.
                Button(onClick = onOpen, enabled = enabled) { Text("Open") }
                TextButton(onClick = onDisconnect, enabled = enabled) { Text("Disconnect") }
            } else {
                Button(onClick = onConnect, enabled = enabled) { Text("Connect") }
                TextButton(onClick = onForget, enabled = enabled) { Text("Forget") }
            }
        }
    }
}

private const val REFRESH_MS = 2000L
