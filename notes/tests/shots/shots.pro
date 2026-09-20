QT += core gui qml quick quickcontrols2 printsupport
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = shots
TEMPLATE = app

SOURCES += shots.cpp \
    ../../src/notesbackend.cpp
HEADERS += ../../src/notesbackend.h
INCLUDEPATH += ../../src
