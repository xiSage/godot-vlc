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
    pub fn from_ptr(ptr: *mut libvlc_media_tracklist_t) -> Option<Gd<Self>> {
        if ptr.is_null() {
            None
        } else {
            Some(Gd::from_object(VlcTrackList { ptr }))
        }
    }

    /// Get a track at a specific index.\
    ///
    /// # Returns
    /// a valid [VLCTrack], or null if the index is out of range.
    #[func]
    fn tracklist_at(&self, index: u64) -> Option<Gd<VlcTrack>> {
        if index >= self.tracklist_count() {
            return None;
        }
        let ptr = unsafe { libvlc_media_tracklist_at(self.ptr, index as usize) };
        let ptr = unsafe { libvlc_media_track_hold(ptr) };
        Some(VlcTrack::from_ptr(ptr))
    }

    /// Get the number of tracks in a tracklist.
    ///
    /// # Returns
    /// number of tracks, or 0 if the list is empty
    #[func]
    fn tracklist_count(&self) -> u64 {
        unsafe { libvlc_media_tracklist_count(self.ptr) as u64 }
    }

    /// Get all tracks in the tracklist.
    #[func]
    fn get_tracks(&self) -> Array<Option<Gd<VlcTrack>>> {
        let count = self.tracklist_count();
        let mut tracks = Array::new();
        for i in 0..count {
            tracks.push(&self.tracklist_at(i));
        }
        tracks
    }
}
