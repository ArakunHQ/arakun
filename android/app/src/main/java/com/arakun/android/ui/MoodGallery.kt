package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** Every mood side by side, for design work: launch with `demo` and `demoMoods`. */
@Composable
fun MoodGallery() {
    val all: List<Triple<Mood, String, Dp>> = Mood.entries.map { Triple(it, it.name.lowercase().replaceFirstChar(Char::uppercase), 120.dp) } +
        Triple(Mood.WAITING, "40 dp", 40.dp)
    LazyVerticalGrid(
        columns = GridCells.Adaptive(150.dp),
        modifier = Modifier.fillMaxSize().background(Palette.current.ground).safeDrawingPadding(),
        contentPadding = PaddingValues(20.dp),
        verticalArrangement = Arrangement.spacedBy(28.dp),
    ) {
        items(all) { (mood, name, width) ->
            Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Mascot(mood, width)
                Eyebrow(name)
            }
        }
    }
}
