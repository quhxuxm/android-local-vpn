package com.ppaass.agent.vpn

import android.os.Parcelable
import kotlinx.parcelize.Parcelize

@Parcelize
data class LocalVpnConfiguration(
    val allowedApps: List<PackageName>? = null,
    val disallowedApps: List<PackageName>? = null
) : Parcelable

@JvmInline
@Parcelize
value class PackageName(val packageName: String) : Parcelable