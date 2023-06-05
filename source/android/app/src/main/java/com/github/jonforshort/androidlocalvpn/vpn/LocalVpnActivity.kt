package com.github.jonforshort.androidlocalvpn.vpn

import android.net.VpnService
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts.StartActivityForResult

abstract class LocalVpnActivity : ComponentActivity() {

    private lateinit var pendingConfiguration: LocalVpnConfiguration

    private val vpnServicePreparedLauncher =
        registerForActivityResult(StartActivityForResult()) { result ->
            if (result.resultCode == RESULT_OK) {
                startVpn(this, pendingConfiguration)
                onVpnStarted()
            }
        }

    protected fun startVpn(configuration: LocalVpnConfiguration = LocalVpnConfiguration()) {
        val vpnIntent = VpnService.prepare(this)
        if (vpnIntent == null) {
            startVpn(this, configuration)
            onVpnStarted()
        } else {
            //
            // Need to remember configuration before starting intent since unable to
            // package our own custom extra data to vpn intent.
            //
            pendingConfiguration = configuration

            vpnServicePreparedLauncher.launch(vpnIntent)
        }
    }

    protected fun stopVpn() {
        stopVpn(this)
        onVpnStopped()
    }

    protected fun isVpnRunning() =
        isVpnRunning(this)

    abstract fun onVpnStarted()

    abstract fun onVpnStopped()
}