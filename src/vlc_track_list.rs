/*
* Copyright (c) 2025 xiSage
*
* This library is free software; you can redistribute it and/or
* modify it under the terms of the GNU Lesser General Public
* License as published by the Free Software Foundation; either
* version 2.1 of the License, or (at your option) any later version.
*
* This library is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
* Lesser General Public License for more details.
*
* You should have received a copy of the GNU Lesser General Public
* License along with this library; if not, write to the Free Software
* Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301
* USA
*/

use crate::{vlc::*, vlc_track::VlcTrack};
use godot::prelude::*;

#[derive(GodotClass)]
#[class(rename=VLCTrackList, no_init)]
pub struct VlcTrackList {
    ptr: *mut libvlc_media_tracklist_t,
    /// Which half of libvlc's track API this list came from: a player's tracklist
    /// or a media descriptor's. It travels with every track taken out of it,
    /// because only a player's track can be selected; see [VlcTrack::from_player].
    from_player: bool,
}

impl Drop for VlcTrackList {
    fn drop(&mut self) {
        unsafe {
            libvlc_media_tracklist_delete(self.ptr);
        }
    }
}

#[godot_api]
impl VlcTrackList {
    pub fn from_ptr(ptr: *mut libvlc_media_tracklist_t, from_player: bool) -> Option<Gd<Self>> {
        if ptr.is_null() {
            None
        } else {
            Some(Gd::from_object(VlcTrackList { ptr, from_player }))
        }
    }

    /// Get a track at a specific index.
    ///
    /// # Returns
    /// a valid [VLCTrack], or null if the index is out of range.
    #[func]
    fn tracklist_at(&self, index: u32) -> Option<Gd<VlcTrack>> {
        if index >= self.tracklist_count() {
            return None;
        }
        let ptr = unsafe { libvlc_media_tracklist_at(self.ptr, index as usize) };
        let ptr = unsafe { libvlc_media_track_hold(ptr) };
        Some(VlcTrack::from_ptr(ptr, self.from_player))
    }

    /// Get the number of tracks in a tracklist.
    ///
    /// # Returns
    /// number of tracks, or 0 if the list is empty
    #[func]
    fn tracklist_count(&self) -> u32 {
        unsafe { libvlc_media_tracklist_count(self.ptr) as u32 }
    }

    /// Get all tracks in the tracklist.
    ///
    /// # Returns
    /// an array of [VLCTrack], empty when the list is. Every element is a valid
    /// track -- this list has no holes -- so the array needs no null check and can
    /// be handed straight to [method VLCMediaPlayer.select_tracks].
    #[func]
    fn get_tracks(&self) -> Array<Gd<VlcTrack>> {
        let count = self.tracklist_count();
        let mut tracks = Array::new();
        for i in 0..count {
            if let Some(track) = self.tracklist_at(i) {
                tracks.push(&track);
            }
        }
        tracks
    }
}
